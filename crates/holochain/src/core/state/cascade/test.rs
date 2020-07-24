use super::Cascade;
use crate::core::state::{
    chain_cas::ChainCasBuf,
    metadata::{EntryDhtStatus, LinkMetaKey, MockMetadataBuf},
    source_chain::{SourceChainBuf, SourceChainResult},
};
use crate::{
    fixt::{LinkMetaValFixturator, ZomeIdFixturator},
    test_utils::test_network,
};
use ::fixt::prelude::*;
use holochain_state::{
    env::ReadManager, error::DatabaseResult, prelude::*, test_utils::test_cell_env,
};
use holochain_types::{
    element::SignedHeaderHashed,
    entry::EntryHashed,
    fixt::SignatureFixturator,
    observability,
    prelude::*,
    test_utils::{fake_agent_pubkey_1, fake_agent_pubkey_2, fake_header_hash},
    HeaderHashed,
};
use holochain_zome_types::link::LinkTag;
use holochain_zome_types::{header, Entry, Header};
use mockall::*;

#[allow(dead_code)]
struct Chains<'env> {
    source_chain: SourceChainBuf<'env>,
    cache: ChainCasBuf<'env>,
    jimbo_id: AgentPubKey,
    jimbo_header: Header,
    jimbo_entry: EntryHashed,
    jessy_id: AgentPubKey,
    jessy_header: Header,
    jessy_entry: EntryHashed,
    mock_primary_meta: MockMetadataBuf,
    mock_cache_meta: MockMetadataBuf,
}

fn setup_env<'env>(reader: &'env Reader<'env>, dbs: &impl GetDb) -> DatabaseResult<Chains<'env>> {
    let previous_header = fake_header_hash(1);

    let jimbo_id = fake_agent_pubkey_1();
    let jessy_id = fake_agent_pubkey_2();
    let (jimbo_entry, jessy_entry) = tokio_safe_block_on::tokio_safe_block_on(
        async {
            (
                EntryHashed::with_data(Entry::Agent(jimbo_id.clone().into()))
                    .await
                    .unwrap(),
                EntryHashed::with_data(Entry::Agent(jessy_id.clone().into()))
                    .await
                    .unwrap(),
            )
        },
        std::time::Duration::from_secs(1),
    )
    .unwrap();

    let jimbo_header = Header::EntryCreate(header::EntryCreate {
        author: jimbo_id.clone(),
        timestamp: Timestamp::now().into(),
        header_seq: 0,
        prev_header: previous_header.clone().into(),
        entry_type: header::EntryType::AgentPubKey,
        entry_hash: jimbo_entry.as_hash().clone(),
    });

    let jessy_header = Header::EntryCreate(header::EntryCreate {
        author: jessy_id.clone(),
        timestamp: Timestamp::now().into(),
        header_seq: 0,
        prev_header: previous_header.clone().into(),
        entry_type: header::EntryType::AgentPubKey,
        entry_hash: jessy_entry.as_hash().clone(),
    });

    let source_chain = SourceChainBuf::new(reader, dbs)?;
    let cache = ChainCasBuf::cache(reader, dbs)?;
    let mock_primary_meta = MockMetadataBuf::new();
    let mock_cache_meta = MockMetadataBuf::new();
    Ok(Chains {
        source_chain,
        cache,
        jimbo_id,
        jimbo_header,
        jimbo_entry,
        jessy_id,
        jessy_header,
        jessy_entry,
        mock_primary_meta,
        mock_cache_meta,
    })
}

#[tokio::test(threaded_scheduler)]
async fn live_local_return() -> SourceChainResult<()> {
    // setup some data thats in the scratch
    let env = test_cell_env();
    let dbs = env.dbs().await;
    let env_ref = env.guard().await;
    let reader = env_ref.reader()?;
    let Chains {
        mut source_chain,
        mut cache,
        jimbo_header,
        jimbo_entry,
        mut mock_primary_meta,
        mock_cache_meta,
        ..
    } = setup_env(&reader, &dbs)?;
    source_chain
        .put_raw(jimbo_header.clone(), Some(jimbo_entry.as_content().clone()))
        .await?;
    let address = jimbo_entry.as_hash();

    // set it's metadata to LIVE
    mock_primary_meta
        .expect_get_dht_status()
        .with(predicate::eq(address.clone()))
        .returning(|_| Ok(EntryDhtStatus::Live));

    let (_n, _r, cell_network) = test_network().await;

    // call dht_get with above address
    let cascade = Cascade::new(
        &source_chain.cas(),
        &mock_primary_meta,
        &mut cache,
        &mock_cache_meta,
        cell_network,
    );
    let entry = cascade.dht_get(address.clone().into()).await?;
    // check it returns
    assert_eq!(entry.unwrap().into_inner().1.unwrap(), *jimbo_entry);
    // check it doesn't hit the cache
    // this is implied by the mock not expecting calls
    Ok(())
}

#[tokio::test(threaded_scheduler)]
async fn dead_local_none() -> SourceChainResult<()> {
    observability::test_run().ok();
    // setup some data thats in the scratch
    let env = test_cell_env();
    let dbs = env.dbs().await;
    let env_ref = env.guard().await;
    let reader = env_ref.reader()?;
    let Chains {
        mut source_chain,
        mut cache,
        jimbo_id: _,
        jimbo_header,
        jimbo_entry,
        mut mock_primary_meta,
        mock_cache_meta,
        ..
    } = setup_env(&reader, &dbs)?;
    source_chain
        .put_raw(jimbo_header.clone(), Some(jimbo_entry.as_content().clone()))
        .await?;
    let address = jimbo_entry.as_hash();

    // set it's metadata to Dead
    mock_primary_meta
        .expect_get_dht_status()
        .with(predicate::eq(address.clone()))
        .returning(|_| Ok(EntryDhtStatus::Dead));

    let (_n, _r, cell_network) = test_network().await;
    // call dht_get with above address
    let cascade = Cascade::new(
        &source_chain.cas(),
        &mock_primary_meta,
        &mut cache,
        &mock_cache_meta,
        cell_network,
    );
    let entry = cascade.dht_get(address.clone().into()).await?;
    // check it returns none
    assert_eq!(entry, None);
    // check it doesn't hit the cache
    // this is implied by the mock not expecting calls
    Ok(())
}

#[tokio::test(threaded_scheduler)]
async fn notfound_goto_cache_live() -> SourceChainResult<()> {
    observability::test_run().ok();
    // setup some data thats in the scratch
    let env = test_cell_env();
    let dbs = env.dbs().await;
    let env_ref = env.guard().await;
    let reader = env_ref.reader()?;
    let Chains {
        source_chain,
        mut cache,
        jimbo_id: _,
        jimbo_header,
        jimbo_entry,
        mock_primary_meta,
        mut mock_cache_meta,
        ..
    } = setup_env(&reader, &dbs)?;
    let h = HeaderHashed::with_data(jimbo_header.clone()).await.unwrap();
    let h = SignedHeaderHashed::with_presigned(h, fixt!(Signature));
    cache.put(h, Some(jimbo_entry.clone()))?;
    let address = jimbo_entry.as_hash();

    // set it's metadata to Live
    mock_cache_meta
        .expect_get_dht_status()
        .with(predicate::eq(address.clone()))
        .returning(|_| Ok(EntryDhtStatus::Live));

    let (_n, _r, cell_network) = test_network().await;
    // call dht_get with above address
    let cascade = Cascade::new(
        &source_chain.cas(),
        &mock_primary_meta,
        &mut cache,
        &mock_cache_meta,
        cell_network,
    );
    let _entry = cascade.dht_get(address.clone().into()).await?;
    // check it returns

    // FIXME!
    //    assert_eq!(entry, Some(jimbo_entry));
    // check it doesn't hit the primary
    // this is implied by the mock not expecting calls
    Ok(())
}

#[tokio::test(threaded_scheduler)]
async fn notfound_cache() -> DatabaseResult<()> {
    observability::test_run().ok();
    // setup some data thats in the scratch
    let env = test_cell_env();
    let dbs = env.dbs().await;
    let env_ref = env.guard().await;
    let reader = env_ref.reader()?;
    let Chains {
        source_chain,
        mut cache,
        jimbo_header: _,
        jimbo_entry,
        mock_primary_meta,
        mock_cache_meta,
        ..
    } = setup_env(&reader, &dbs)?;
    let address = jimbo_entry.as_hash();

    let (_n, _r, cell_network) = test_network().await;
    // call dht_get with above address
    let cascade = Cascade::new(
        &source_chain.cas(),
        &mock_primary_meta,
        &mut cache,
        &mock_cache_meta,
        cell_network,
    );
    let entry = cascade.dht_get(address.clone().into()).await?;
    // check it returns
    assert_eq!(entry, None);
    // check it doesn't hit the primary
    // this is implied by the mock not expecting calls
    // check it doesn't ask the cache
    // this is implied by the mock not expecting calls
    Ok(())
}

#[tokio::test(threaded_scheduler)]
async fn links_local_return() -> SourceChainResult<()> {
    // setup some data thats in the scratch
    let env = test_cell_env();
    let dbs = env.dbs().await;
    let env_ref = env.guard().await;
    let reader = env_ref.reader()?;
    let Chains {
        mut source_chain,
        mut cache,
        jimbo_id: _,
        jimbo_header,
        jimbo_entry,
        jessy_id: _,
        jessy_header,
        jessy_entry,
        mut mock_primary_meta,
        mock_cache_meta,
    } = setup_env(&reader, &dbs)?;
    source_chain
        .put_raw(jimbo_header.clone(), Some(jimbo_entry.as_content().clone()))
        .await?;
    source_chain
        .put_raw(jessy_header.clone(), Some(jessy_entry.as_content().clone()))
        .await?;
    let base = jimbo_entry.as_hash().clone();
    let target = jessy_entry.as_hash().clone();

    let tag = LinkTag::new(BytesFixturator::new(Unpredictable).next().unwrap());
    let zome_id = ZomeIdFixturator::new(Unpredictable).next().unwrap();

    let link = LinkMetaValFixturator::new((target.clone(), tag.clone()))
        .next()
        .unwrap();

    let key = LinkMetaKey::BaseZomeTag(&base, zome_id, &tag);

    // Return a link between entries
    let link_return = vec![link.clone()];
    mock_primary_meta
        .expect_get_links()
        .withf({
            let base = base.clone();
            let tag = tag.clone();
            move |k| {
                let key = LinkMetaKey::BaseZomeTag(&base, zome_id, &tag);
                k == &key
            }
        })
        .returning({
            move |_| {
                Ok(Box::new(fallible_iterator::convert(
                    link_return.clone().into_iter().map(Ok),
                )))
            }
        });

    let (_n, _r, cell_network) = test_network().await;
    // call dht_get_links with above base
    let cascade = Cascade::new(
        &source_chain.cas(),
        &mock_primary_meta,
        &mut cache,
        &mock_cache_meta,
        cell_network,
    );
    let links = cascade.dht_get_links(&key).await?;
    // check it returns
    assert_eq!(links, vec![link.into_link()]);
    // check it doesn't hit the cache
    // this is implied by the mock not expecting calls
    Ok(())
}

#[tokio::test(threaded_scheduler)]
async fn links_cache_return() -> SourceChainResult<()> {
    observability::test_run().ok();
    // setup some data thats in the scratch
    let env = test_cell_env();
    let dbs = env.dbs().await;
    let env_ref = env.guard().await;
    let reader = env_ref.reader()?;
    let Chains {
        mut source_chain,
        mut cache,
        jimbo_id: _,
        jimbo_header,
        jimbo_entry,
        jessy_id: _,
        jessy_header,
        jessy_entry,
        mut mock_primary_meta,
        mut mock_cache_meta,
    } = setup_env(&reader, &dbs)?;
    source_chain
        .put_raw(jimbo_header.clone(), Some(jimbo_entry.as_content().clone()))
        .await?;
    source_chain
        .put_raw(jessy_header.clone(), Some(jessy_entry.as_content().clone()))
        .await?;
    let base = jimbo_entry.as_hash().clone();
    let target = jessy_entry.as_hash().clone();

    let tag = LinkTag::new(BytesFixturator::new(Unpredictable).next().unwrap());
    let zome_id = ZomeIdFixturator::new(Unpredictable).next().unwrap();

    let link = LinkMetaValFixturator::new((target.clone(), tag.clone()))
        .next()
        .unwrap();

    let key = LinkMetaKey::BaseZomeTag(&base, zome_id, &tag);

    let link_return = vec![];
    // Return empty links
    mock_primary_meta
        .expect_get_links()
        .withf({
            let base = base.clone();
            let tag = tag.clone();
            move |k| {
                let key = LinkMetaKey::BaseZomeTag(&base, zome_id, &tag);
                k == &key
            }
        })
        .returning({
            move |_| {
                Ok(Box::new(fallible_iterator::convert(
                    link_return.clone().into_iter().map(Ok),
                )))
            }
        });

    let link_return = vec![link.clone()];
    // Return a link between entries
    mock_cache_meta
        .expect_get_links()
        .withf({
            let base = base.clone();
            let tag = tag.clone();
            move |k| {
                let key = LinkMetaKey::BaseZomeTag(&base, zome_id, &tag);
                k == &key
            }
        })
        .returning({
            move |_| {
                Ok(Box::new(fallible_iterator::convert(
                    link_return.clone().into_iter().map(Ok),
                )))
            }
        });

    let (_n, _r, cell_network) = test_network().await;
    // call dht_get_links with above base
    let cascade = Cascade::new(
        &source_chain.cas(),
        &mock_primary_meta,
        &mut cache,
        &mock_cache_meta,
        cell_network,
    );
    let links = cascade.dht_get_links(&key).await?;
    // check it returns
    assert_eq!(links, vec![link.into_link()]);
    Ok(())
}

#[tokio::test(threaded_scheduler)]
async fn links_notauth_cache() -> DatabaseResult<()> {
    observability::test_run().ok();
    // setup some data thats in the scratch
    let env = test_cell_env();
    let dbs = env.dbs().await;
    let env_ref = env.guard().await;
    let reader = env_ref.reader()?;
    let Chains {
        source_chain,
        mut cache,
        jimbo_header: _,
        jimbo_entry,
        jessy_id: _,
        jessy_header: _,
        jessy_entry,
        mock_primary_meta,
        mut mock_cache_meta,
        ..
    } = setup_env(&reader, &dbs)?;

    let base = jimbo_entry.as_hash().clone();
    let target = jessy_entry.as_hash().clone();

    let tag = LinkTag::new(BytesFixturator::new(Unpredictable).next().unwrap());
    let zome_id = ZomeIdFixturator::new(Unpredictable).next().unwrap();

    let link = LinkMetaValFixturator::new((target.clone(), tag.clone()))
        .next()
        .unwrap();

    let key = LinkMetaKey::BaseZomeTag(&base, zome_id, &tag);

    let link_return = vec![link.clone()];

    // Return empty links
    mock_cache_meta
        .expect_get_links()
        .withf({
            let base = base.clone();
            let tag = tag.clone();
            move |k| {
                let key = LinkMetaKey::BaseZomeTag(&base, zome_id, &tag);
                k == &key
            }
        })
        .returning({
            move |_| {
                Ok(Box::new(fallible_iterator::convert(
                    link_return.clone().into_iter().map(Ok),
                )))
            }
        });

    let (_n, _r, cell_network) = test_network().await;

    // call dht_get_links with above base
    let cascade = Cascade::new(
        &source_chain.cas(),
        &mock_primary_meta,
        &mut cache,
        &mock_cache_meta,
        cell_network,
    );
    let links = cascade.dht_get_links(&key).await?;
    // check it returns
    assert_eq!(links, vec![link.into_link()]);
    // check it doesn't hit the primary
    // this is implied by the mock not expecting calls
    Ok(())
}
