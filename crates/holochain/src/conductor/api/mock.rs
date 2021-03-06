#![allow(missing_docs)]
#![allow(clippy::ptr_arg)]

use super::CellConductorApiT;
use crate::conductor::{api::error::ConductorApiResult, entry_def_store::EntryDefBufferKey};
use crate::core::ribosome::ZomeCallInvocation;
use crate::core::workflow::ZomeCallInvocationResult;
use async_trait::async_trait;
use holo_hash::DnaHash;
use holochain_keystore::KeystoreSender;
use holochain_types::dna::DnaFile;
use holochain_types::{autonomic::AutonomicCue, cell::CellId};
use holochain_zome_types::entry_def::EntryDef;
use mockall::mock;

// Unfortunate workaround to get mockall to work with async_trait, due to the complexity of each.
// The mock! expansion here creates mocks on a non-async version of the API, and then the actual trait is implemented
// by delegating each async trait method to its sync counterpart
// See https://github.com/asomers/mockall/issues/75
mock! {

    pub CellConductorApi {
        fn cell_id(&self) -> &CellId;
        fn sync_call_zome(
            &self,
            cell_id: &CellId,
            invocation: ZomeCallInvocation,
        ) -> ConductorApiResult<ZomeCallInvocationResult>;

        fn sync_autonomic_cue(&self, cue: AutonomicCue) -> ConductorApiResult<()>;

        fn sync_dpki_request(&self, method: String, args: String) -> ConductorApiResult<String>;

        fn mock_keystore(&self) -> &KeystoreSender;
        fn sync_get_dna(&self, dna_hash: &DnaHash) -> Option<DnaFile>;
        fn sync_get_this_dna(&self) -> Option<DnaFile>;
        fn sync_get_entry_def(&self, key: &EntryDefBufferKey) -> Option<EntryDef>;
    }

    trait Clone {
        fn clone(&self) -> Self;
    }
}

#[async_trait]
impl CellConductorApiT for MockCellConductorApi {
    fn cell_id(&self) -> &CellId {
        self.cell_id()
    }

    async fn call_zome(
        &self,
        cell_id: &CellId,
        invocation: ZomeCallInvocation,
    ) -> ConductorApiResult<ZomeCallInvocationResult> {
        self.sync_call_zome(cell_id, invocation)
    }

    async fn dpki_request(&self, method: String, args: String) -> ConductorApiResult<String> {
        self.sync_dpki_request(method, args)
    }

    async fn autonomic_cue(&self, cue: AutonomicCue) -> ConductorApiResult<()> {
        self.sync_autonomic_cue(cue)
    }

    fn keystore(&self) -> &KeystoreSender {
        self.mock_keystore()
    }
    async fn get_dna(&self, dna_hash: &DnaHash) -> Option<DnaFile> {
        self.sync_get_dna(dna_hash)
    }
    async fn get_this_dna(&self) -> Option<DnaFile> {
        self.sync_get_this_dna()
    }
    async fn get_entry_def(&self, key: &EntryDefBufferKey) -> Option<EntryDef> {
        self.sync_get_entry_def(key)
    }
}
