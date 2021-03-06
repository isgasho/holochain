//! # Cascade
//! ## Retrieve vs Get
//! Get checks CRUD metadata before returning an the data
//! where as retrieve only checks that where the data was found
//! the appropriate validation has been run.

use super::{
    element_buf::ElementBuf,
    metadata::{LinkMetaKey, MetadataBuf, MetadataBufT, SysMetaVal},
};
use crate::core::workflow::{
    integrate_dht_ops_workflow::integrate_single_metadata,
    produce_dht_ops_workflow::dht_op_light::error::DhtOpConvertResult,
};
use error::CascadeResult;
use fallible_iterator::FallibleIterator;
use holo_hash::{
    hash_type::{self, AnyDht},
    AnyDhtHash, EntryHash, HasHash, HeaderHash,
};
use holochain_p2p::HolochainP2pCellT;
use holochain_p2p::{
    actor::{GetLinksOptions, GetMetaOptions, GetOptions},
    HolochainP2pCell,
};
use holochain_state::{error::DatabaseResult, fresh_reader, prelude::*};
use holochain_types::{
    dht_op::{produce_op_lights_from_element_group, produce_op_lights_from_elements},
    element::{
        Element, ElementGroup, GetElementResponse, RawGetEntryResponse, SignedHeaderHashed,
        SignedHeaderHashedExt,
    },
    entry::option_entry_hashed,
    link::{GetLinksResponse, WireLinkMetaKey},
    metadata::{EntryDhtStatus, MetadataSet, TimedHeaderHash},
    EntryHashed, HeaderHashed,
};
use holochain_zome_types::header::{CreateLink, DeleteLink};
use holochain_zome_types::{
    element::SignedHeader,
    header::{Delete, Update},
    link::Link,
    metadata::{Details, ElementDetails, EntryDetails},
    Header,
};
use std::convert::TryFrom;
use std::{
    collections::{BTreeMap, BTreeSet},
    convert::TryInto,
};
use tracing::*;
use tracing_futures::Instrument;

#[cfg(test)]
mod network_tests;
#[cfg(all(test, outdated_tests))]
mod test;

pub mod error;

pub struct Cascade<'a, Network = HolochainP2pCell, MetaVault = MetadataBuf, MetaCache = MetadataBuf>
where
    Network: HolochainP2pCellT,
    MetaVault: MetadataBufT,
    MetaCache: MetadataBufT,
{
    element_vault: &'a ElementBuf,
    meta_vault: &'a MetaVault,

    element_cache: &'a mut ElementBuf,
    meta_cache: &'a mut MetaCache,

    env: EnvironmentRead,
    network: Network,
}

#[derive(Debug)]
/// The state of the cascade search
enum Search {
    /// The entry is found and we can stop
    Found(Element),
    /// We haven't found the entry yet and should
    /// continue searching down the cascade
    Continue(HeaderHash),
    /// We haven't found the entry and should
    /// not continue searching down the cascade
    // TODO This information is currently not passed back to
    // the caller however it might be useful.
    NotInCascade,
}

/// Should these functions be sync or async?
/// Depends on how much computation, and if writes are involved
impl<'a, Network, MetaVault, MetaCache> Cascade<'a, Network, MetaVault, MetaCache>
where
    MetaCache: MetadataBufT,
    MetaVault: MetadataBufT,
    Network: HolochainP2pCellT,
{
    /// Constructs a [Cascade], taking references to all necessary databases
    pub fn new(
        env: EnvironmentRead,
        element_vault: &'a ElementBuf,
        meta_vault: &'a MetaVault,
        element_cache: &'a mut ElementBuf,
        meta_cache: &'a mut MetaCache,
        network: Network,
    ) -> Self {
        Cascade {
            env,
            element_vault,
            meta_vault,
            element_cache,
            meta_cache,
            network,
        }
    }

    async fn update_stores(&mut self, element: Element) -> CascadeResult<()> {
        let op_lights = produce_op_lights_from_elements(vec![&element]).await?;
        let (shh, e) = element.into_inner();
        self.element_cache.put(shh, option_entry_hashed(e).await)?;
        for op in op_lights {
            integrate_single_metadata(op, &self.element_cache, self.meta_cache)?
        }
        Ok(())
    }

    #[instrument(skip(self, elements))]
    async fn update_stores_with_element_group(
        &mut self,
        elements: ElementGroup<'_>,
    ) -> CascadeResult<()> {
        let op_lights = produce_op_lights_from_element_group(&elements).await?;
        self.element_cache.put_element_group(elements)?;
        for op in op_lights {
            integrate_single_metadata(op, &self.element_cache, self.meta_cache)?
        }
        Ok(())
    }

    async fn fetch_element_via_header(
        &mut self,
        hash: HeaderHash,
        options: GetOptions,
    ) -> CascadeResult<()> {
        let results = self.network.get(hash.into(), options).await?;
        // Search through the returns for the first delete
        for response in results.into_iter() {
            match response {
                // Has header
                GetElementResponse::GetHeader(Some(we)) => {
                    let (element, delete) = we.into_element_and_delete().await;
                    self.update_stores(element).await?;

                    if let Some(delete) = delete {
                        self.update_stores(delete).await?;
                    }
                }
                // Doesn't have header but not because it was deleted
                GetElementResponse::GetHeader(None) => (),
                r => {
                    error!(
                        msg = "Got an invalid response to fetch element via header",
                        ?r
                    );
                }
            }
        }
        Ok(())
    }

    #[instrument(skip(self, options))]
    async fn fetch_element_via_entry(
        &mut self,
        hash: EntryHash,
        options: GetOptions,
    ) -> CascadeResult<()> {
        let results = self
            .network
            .get(hash.clone().into(), options.clone())
            .instrument(debug_span!("fetch_element_via_entry::network_get"))
            .await?;

        for response in results {
            match response {
                GetElementResponse::GetEntryFull(Some(raw)) => {
                    let RawGetEntryResponse {
                        live_headers,
                        deletes,
                        entry,
                        entry_type,
                        updates,
                    } = *raw;
                    let elements =
                        ElementGroup::from_wire_elements(live_headers, entry_type, entry).await?;
                    let entry_hash = elements.entry_hash().clone();
                    self.update_stores_with_element_group(elements).await?;
                    for delete in deletes {
                        let element = delete.into_element().await;
                        self.update_stores(element).await?;
                    }
                    for update in updates {
                        let element = update.into_element(entry_hash.clone()).await;
                        self.update_stores(element).await?;
                    }
                }
                // Authority didn't have any headers for this entry
                GetElementResponse::GetEntryFull(None) => (),
                r @ GetElementResponse::GetHeader(_) => {
                    error!(
                        msg = "Got an invalid response to fetch element via entry",
                        ?r
                    );
                }
                r => unimplemented!("{:?} is unimplemented for fetching via entry", r),
            }
        }
        Ok(())
    }

    // TODO: Remove when used
    #[allow(dead_code)]
    async fn fetch_meta(
        &mut self,
        basis: AnyDhtHash,
        options: GetMetaOptions,
    ) -> CascadeResult<Vec<MetadataSet>> {
        let all_metadata = self.network.get_meta(basis.clone(), options).await?;

        // Only put raw meta data in element_cache and combine all results
        for metadata in <[_]>::iter(&all_metadata[..]).cloned() {
            let basis = basis.clone();
            // Put in meta element_cache
            let values = metadata
                .headers
                .into_iter()
                .map(SysMetaVal::NewEntry)
                .chain(metadata.deletes.into_iter().map(SysMetaVal::Delete))
                .chain(metadata.updates.into_iter().map(SysMetaVal::Update));
            match *basis.hash_type() {
                hash_type::AnyDht::Entry => {
                    for v in values {
                        self.meta_cache
                            .register_raw_on_entry(basis.clone().into(), v)?;
                    }
                }
                hash_type::AnyDht::Header => {
                    for v in values {
                        self.meta_cache
                            .register_raw_on_header(basis.clone().into(), v);
                    }
                }
            }
        }
        Ok(all_metadata)
    }

    #[instrument(skip(self, options))]
    async fn fetch_links(
        &mut self,
        link_key: WireLinkMetaKey,
        options: GetLinksOptions,
    ) -> CascadeResult<()> {
        debug!("in get links");
        let results = self.network.get_links(link_key, options).await?;
        for links in results {
            let GetLinksResponse {
                link_adds,
                link_removes,
            } = links;

            for (link_add, signature) in link_adds {
                debug!(?link_add);
                let element = Element::new(
                    SignedHeaderHashed::from_content_sync(SignedHeader(link_add.into(), signature)),
                    None,
                );
                self.update_stores(element).await?;
            }
            for (link_remove, signature) in link_removes {
                debug!(?link_remove);
                let element = Element::new(
                    SignedHeaderHashed::from_content_sync(SignedHeader(
                        link_remove.into(),
                        signature,
                    )),
                    None,
                );
                self.update_stores(element).await?;
            }
        }
        Ok(())
    }

    fn get_element_local_raw(&self, hash: &HeaderHash) -> CascadeResult<Option<Element>> {
        let r = match self.element_vault.get_element(hash)? {
            None => self.element_cache.get_element(hash)?,
            r => r,
        };
        // Check we have a valid reason to return this element
        match r {
            Some(el)
                if self.valid_element(
                    el.header_address(),
                    el.header().entry_data().map(|(h, _)| h),
                )? =>
            {
                Ok(Some(el))
            }
            _ => Ok(None),
        }
    }

    /// Gets the first element we can find for this entry locally
    fn get_element_local_raw_via_entry(&self, hash: &EntryHash) -> CascadeResult<Option<Element>> {
        // Get all the headers we know about.
        let mut headers: BTreeSet<TimedHeaderHash> =
            fresh_reader!(self.meta_cache.env(), |r| self
                .meta_cache
                .get_headers(&r, hash.clone())?
                .collect())?;
        headers.extend(fresh_reader!(self.meta_cache.env(), |r| self
            .meta_vault
            .get_headers(&r, hash.clone())?
            .collect::<Vec<_>>())?);

        // We might not actually be holding some of these
        // so we need to search until we find one.
        // We are most likely holding the newest header
        // so iterate in reverse
        for header in headers.into_iter().rev() {
            // Return the first element we are actually holding
            if let Some(el) = self.get_element_local_raw(&header.header_hash)? {
                return Ok(Some(el));
            }
        }
        // Not holding any
        Ok(None)
    }

    fn get_entry_local_raw(&self, hash: &EntryHash) -> CascadeResult<Option<EntryHashed>> {
        let r = match self.element_vault.get_entry(hash)? {
            None => self.element_cache.get_entry(hash)?,
            r => r,
        };
        // Check we have a valid reason to return this element
        match r {
            Some(e) if self.valid_entry(e.as_hash())? => Ok(Some(e)),
            _ => Ok(None),
        }
    }

    fn get_header_local_raw(&self, hash: &HeaderHash) -> CascadeResult<Option<HeaderHashed>> {
        Ok(self
            .get_header_local_raw_with_sig(hash)?
            .map(|h| h.into_header_and_signature().0))
    }

    fn get_header_local_raw_with_sig(
        &self,
        hash: &HeaderHash,
    ) -> CascadeResult<Option<SignedHeaderHashed>> {
        let r = match self.element_vault.get_header(hash)? {
            None => self.element_cache.get_header(hash)?,
            r => r,
        };
        // Check we have a valid reason to return this element
        match r {
            Some(h)
                if self.valid_element(
                    h.header_address(),
                    h.header().entry_data().map(|(h, _)| h),
                )? =>
            {
                Ok(Some(h))
            }
            _ => Ok(None),
        }
    }

    fn render_headers<T, F>(&self, headers: Vec<TimedHeaderHash>, f: F) -> CascadeResult<Vec<T>>
    where
        F: Fn(Header) -> DhtOpConvertResult<T>,
    {
        let mut result = Vec::with_capacity(headers.len());
        for h in headers {
            let hash = h.header_hash;
            let h = self.get_header_local_raw(&hash)?;
            match h {
                Some(h) => result.push(f(HeaderHashed::into_content(h))?),
                None => continue,
            }
        }
        Ok(result)
    }

    async fn create_entry_details(&self, hash: EntryHash) -> CascadeResult<Option<EntryDetails>> {
        match self.get_entry_local_raw(&hash)? {
            Some(entry) => fresh_reader!(self.env, |r| {
                let entry_dht_status = self.meta_cache.get_dht_status(&r, &hash)?;
                let headers = self
                    .meta_cache
                    .get_headers(&r, hash.clone())?
                    .collect::<Vec<_>>()?;
                let headers = self.render_headers(headers, Ok)?;
                let deletes = self
                    .meta_cache
                    .get_deletes_on_entry(&r, hash.clone())?
                    .collect::<Vec<_>>()?;
                let deletes = self.render_headers(deletes, |h| Ok(Delete::try_from(h)?))?;
                let updates = self
                    .meta_cache
                    .get_updates(&r, hash.into())?
                    .collect::<Vec<_>>()?;
                let updates = self.render_headers(updates, |h| Ok(Update::try_from(h)?))?;
                Ok(Some(EntryDetails {
                    entry: entry.into_content(),
                    headers,
                    deletes,
                    updates,
                    entry_dht_status,
                }))
            }),
            None => Ok(None),
        }
    }

    fn create_element_details(&self, hash: HeaderHash) -> CascadeResult<Option<ElementDetails>> {
        match self.get_element_local_raw(&hash)? {
            Some(element) => {
                let hash = element.header_address().clone();
                let deletes = fresh_reader!(self.env, |r| self
                    .meta_cache
                    .get_deletes_on_header(&r, hash)?
                    .collect::<Vec<_>>())?;
                let deletes = self.render_headers(deletes, |h| Ok(Delete::try_from(h)?))?;
                Ok(Some(ElementDetails { element, deletes }))
            }
            None => Ok(None),
        }
    }

    fn valid_header(&self, hash: &HeaderHash) -> CascadeResult<bool> {
        Ok(self.meta_vault.has_registered_store_element(&hash)?
            || self.meta_cache.has_registered_store_element(&hash)?)
    }

    fn valid_entry(&self, hash: &EntryHash) -> CascadeResult<bool> {
        if self.meta_cache.has_any_registered_store_entry(hash)? {
            // Found a entry header in the cache
            return Ok(true);
        }
        if self.meta_vault.has_any_registered_store_entry(hash)? {
            // Found a entry header in the vault
            return Ok(true);
        }
        Ok(false)
    }

    /// Check if we have a valid reason to return an element from the cascade
    fn valid_element(
        &self,
        header_hash: &HeaderHash,
        entry_hash: Option<&EntryHash>,
    ) -> CascadeResult<bool> {
        if self.valid_header(&header_hash)? {
            return Ok(true);
        }
        if let Some(eh) = entry_hash {
            if self
                .meta_cache
                .has_registered_store_entry(eh, header_hash)?
            {
                // Found a entry header in the cache
                return Ok(true);
            }
            if self
                .meta_vault
                .has_registered_store_entry(eh, header_hash)?
            {
                // Found a entry header in the vault
                return Ok(true);
            }
        }
        Ok(false)
    }

    #[instrument(skip(self, options))]
    pub async fn get_entry_details(
        &mut self,
        entry_hash: EntryHash,
        options: GetOptions,
    ) -> CascadeResult<Option<EntryDetails>> {
        debug!("in get entry details");
        // Update the cache from the network
        self.fetch_element_via_entry(entry_hash.clone(), options.clone())
            .await?;

        // Get the entry and metadata
        self.create_entry_details(entry_hash).await
    }

    #[instrument(skip(self, options))]
    /// Returns the oldest live [Element] for this [EntryHash] by getting the
    /// latest available metadata from authorities combined with this agents authored data.
    pub async fn dht_get_entry(
        &mut self,
        entry_hash: EntryHash,
        options: GetOptions,
    ) -> CascadeResult<Option<Element>> {
        debug!("in get entry");
        // Update the cache from the network
        self.fetch_element_via_entry(entry_hash.clone(), options.clone())
            .await?;

        // Meta Cache
        let oldest_live_element = fresh_reader!(self.env, |r| {
            match self.meta_cache.get_dht_status(&r, &entry_hash)? {
                EntryDhtStatus::Live => {
                    let oldest_live_header = self
                        .meta_cache
                        .get_headers(&r, entry_hash)?
                        .filter_map(|header| {
                            if self
                                .meta_cache
                                .get_deletes_on_header(&r, header.header_hash.clone())?
                                .next()?
                                .is_none()
                            {
                                Ok(Some(header))
                            } else {
                                Ok(None)
                            }
                        })
                        .min()?
                        .expect("Status is live but no headers?");

                    // We have an oldest live header now get the element
                    CascadeResult::Ok(
                        self.get_element_local_raw(&oldest_live_header.header_hash)?
                            .map(Search::Found)
                            // It's not local so check the network
                            .unwrap_or(Search::Continue(oldest_live_header.header_hash)),
                    )
                }
                EntryDhtStatus::Dead
                | EntryDhtStatus::Pending
                | EntryDhtStatus::Rejected
                | EntryDhtStatus::Abandoned
                | EntryDhtStatus::Conflict
                | EntryDhtStatus::Withdrawn
                | EntryDhtStatus::Purged => CascadeResult::Ok(Search::NotInCascade),
            }
        })?;

        // Network
        match oldest_live_element {
            Search::Found(element) => Ok(Some(element)),
            Search::Continue(oldest_live_header) => {
                self.dht_get_header(oldest_live_header, options).await
            }
            Search::NotInCascade => Ok(None),
        }
    }

    #[instrument(skip(self, options))]
    pub async fn get_header_details(
        &mut self,
        header_hash: HeaderHash,
        options: GetOptions,
    ) -> CascadeResult<Option<ElementDetails>> {
        debug!("in get header details");
        // Network
        self.fetch_element_via_header(header_hash.clone(), options)
            .await?;

        // Get the element and the metadata
        self.create_element_details(header_hash)
    }

    #[instrument(skip(self, options))]
    /// Returns the [Element] for this [HeaderHash] if it is live
    /// by getting the latest available metadata from authorities
    /// combined with this agents authored data.
    /// _Note: Deleted headers are a tombstone set_
    pub async fn dht_get_header(
        &mut self,
        header_hash: HeaderHash,
        options: GetOptions,
    ) -> CascadeResult<Option<Element>> {
        debug!("in get header");
        let found_local_delete = fresh_reader!(self.env, |r| {
            let in_cache = || {
                DatabaseResult::Ok({
                    self.meta_cache
                        .get_deletes_on_header(&r, header_hash.clone())?
                        .next()?
                        .is_some()
                })
            };
            let in_vault = || {
                DatabaseResult::Ok({
                    self.meta_vault
                        .get_deletes_on_header(&r, header_hash.clone())?
                        .next()?
                        .is_some()
                })
            };
            DatabaseResult::Ok(in_cache()? || in_vault()?)
        })?;
        if found_local_delete {
            return Ok(None);
        }
        // Network
        self.fetch_element_via_header(header_hash.clone(), options)
            .await?;

        fresh_reader!(self.env, |r| {
            // Check if header is alive after fetch
            let is_live = self
                .meta_cache
                .get_deletes_on_header(&r, header_hash.clone())?
                .next()?
                .is_none();

            if is_live {
                self.get_element_local_raw(&header_hash)
            } else {
                Ok(None)
            }
        })
    }

    /// Get the entry from the dht regardless of metadata.
    /// This call has the opportunity to hit the local cache
    /// and avoid a network call.
    // TODO: This still fetches the full element and metadata.
    // Need to add a fetch_retrieve_entry that only gets data.
    pub async fn retrieve_entry(
        &mut self,
        hash: EntryHash,
        options: GetOptions,
    ) -> CascadeResult<Option<EntryHashed>> {
        match self.get_entry_local_raw(&hash)? {
            Some(e) => Ok(Some(e)),
            None => {
                self.fetch_element_via_entry(hash.clone(), options).await?;
                self.get_entry_local_raw(&hash)
            }
        }
    }

    /// Get only the header from the dht regardless of metadata.
    /// Useful for avoiding getting the Entry if you don't need it.
    /// This call has the opportunity to hit the local cache
    /// and avoid a network call.
    // TODO: This still fetches the full element and metadata.
    // Need to add a fetch_retrieve_header that only gets data.
    pub async fn retrieve_header(
        &mut self,
        hash: HeaderHash,
        options: GetOptions,
    ) -> CascadeResult<Option<SignedHeaderHashed>> {
        match self.get_header_local_raw_with_sig(&hash)? {
            Some(h) => Ok(Some(h)),
            None => {
                self.fetch_element_via_header(hash.clone(), options).await?;
                self.get_header_local_raw_with_sig(&hash)
            }
        }
    }

    /// Get an element from the dht regardless of metadata.
    /// Useful for checking if data is held.
    /// This call has the opportunity to hit the local cache
    /// and avoid a network call.
    /// Note we still need to return the element as proof they are really
    /// holding it unless we create a byte challenge function.
    // TODO: This still fetches the full element and metadata.
    // Need to add a fetch_retrieve that only gets data.
    pub async fn retrieve(
        &mut self,
        hash: AnyDhtHash,
        options: GetOptions,
    ) -> CascadeResult<Option<Element>> {
        match *hash.hash_type() {
            AnyDht::Entry => {
                let hash = hash.into();
                match self.get_element_local_raw_via_entry(&hash)? {
                    Some(e) => Ok(Some(e)),
                    None => {
                        self.fetch_element_via_entry(hash.clone(), options).await?;
                        self.get_element_local_raw_via_entry(&hash)
                    }
                }
            }
            AnyDht::Header => {
                let hash = hash.into();
                match self.get_element_local_raw(&hash)? {
                    Some(e) => Ok(Some(e)),
                    None => {
                        self.fetch_element_via_header(hash.clone(), options).await?;
                        self.get_element_local_raw(&hash)
                    }
                }
            }
        }
    }

    #[instrument(skip(self))]
    /// Updates the cache with the latest network authority data
    /// and returns what is in the cache.
    /// This gives you the latest possible picture of the current dht state.
    /// Data from your zome call is also added to the cache.
    pub async fn dht_get(
        &mut self,
        hash: AnyDhtHash,
        options: GetOptions,
    ) -> CascadeResult<Option<Element>> {
        match *hash.hash_type() {
            AnyDht::Entry => self.dht_get_entry(hash.into(), options).await,
            AnyDht::Header => self.dht_get_header(hash.into(), options).await,
        }
    }

    #[instrument(skip(self))]
    pub async fn get_details(
        &mut self,
        hash: AnyDhtHash,
        mut options: GetOptions,
    ) -> CascadeResult<Option<Details>> {
        options.all_live_headers_with_metadata = true;
        match *hash.hash_type() {
            AnyDht::Entry => Ok(self
                .get_entry_details(hash.into(), options)
                .await?
                .map(Details::Entry)),
            AnyDht::Header => Ok(self
                .get_header_details(hash.into(), options)
                .await?
                .map(Details::Element)),
        }
    }

    #[instrument(skip(self, key, options))]
    /// Gets an links from the cas or cache depending on it's metadata
    // The default behavior is to skip deleted or replaced entries.
    // TODO: Implement customization of this behavior with an options/builder struct
    pub async fn dht_get_links<'link>(
        &mut self,
        key: &'link LinkMetaKey<'link>,
        options: GetLinksOptions,
    ) -> CascadeResult<Vec<Link>> {
        // Update the cache from the network
        self.fetch_links(key.into(), options).await?;

        fresh_reader!(self.env, |r| {
            // Meta Cache
            // Return any links from the meta cache that don't have removes.
            Ok(self
                .meta_cache
                .get_live_links(&r, key)?
                .map(|l| Ok(l.into_link()))
                .collect()?)
        })
    }

    #[instrument(skip(self, key, options))]
    /// Return all CreateLink headers
    /// and DeleteLink headers ordered by time.
    pub async fn get_link_details<'link>(
        &mut self,
        key: &'link LinkMetaKey<'link>,
        options: GetLinksOptions,
    ) -> CascadeResult<Vec<(CreateLink, Vec<DeleteLink>)>> {
        // Update the cache from the network
        self.fetch_links(key.into(), options).await?;

        // Get the links and collect the CreateLink / DeleteLink hashes by time.
        let links = fresh_reader!(self.env, |r| {
            self.meta_cache
                .get_links_all(&r, key)?
                .map(|link_add| {
                    // Collect the link removes on this link add
                    let link_removes = self
                        .meta_cache
                        .get_link_removes_on_link_add(&r, link_add.link_add_hash.clone())?
                        .collect::<BTreeSet<_>>()?;
                    // Create timed header hash
                    let link_add = TimedHeaderHash {
                        timestamp: link_add.timestamp,
                        header_hash: link_add.link_add_hash,
                    };
                    // Return all link removes with this link add
                    Ok((link_add, link_removes))
                })
                .collect::<BTreeMap<_, _>>()
        })?;
        // Get the headers from the element stores
        let mut result: Vec<(CreateLink, _)> = Vec::with_capacity(links.len());
        for (link_add, link_removes) in links {
            if let Some(link_add) = self.get_element_local_raw(&link_add.header_hash)? {
                let mut r: Vec<DeleteLink> = Vec::with_capacity(link_removes.len());
                for link_remove in link_removes {
                    if let Some(link_remove) =
                        self.get_element_local_raw(&link_remove.header_hash)?
                    {
                        r.push(link_remove.try_into()?);
                    }
                }
                result.push((link_add.try_into()?, r));
            }
        }
        Ok(result)
    }
}

#[cfg(test)]
/// Helper function for easily setting up cascades during tests
pub fn test_dbs_and_mocks(
    env: EnvironmentRead,
) -> (
    ElementBuf,
    super::metadata::MockMetadataBuf,
    ElementBuf,
    super::metadata::MockMetadataBuf,
) {
    let cas = ElementBuf::vault(env.clone().into(), true).unwrap();
    let element_cache = ElementBuf::cache(env.clone().into()).unwrap();
    let metadata = super::metadata::MockMetadataBuf::new();
    let metadata_cache = super::metadata::MockMetadataBuf::new();
    (cas, metadata, element_cache, metadata_cache)
}
