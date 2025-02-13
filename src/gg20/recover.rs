//! This module handles the recover gRPC.
//! Request includes [proto::message_in::Data::KeygenInit] struct and encrypted recovery info.
//! The recovery info is decrypted by party's mnemonic seed and saved in the KvStore.

use super::{keygen::types::KeygenInitSanitized, proto, service::Gg20Service, types::PartyInfo};
use tofn::{
    collections::TypedUsize,
    gg20::keygen::{
        recover_party_keypair, recover_party_keypair_unsafe, KeygenPartyId, SecretKeyShare,
        SecretRecoveryKey,
    },
    sdk::api::{deserialize, BytesVec, PartyShareCounts},
};

// logging
use tracing::{info, warn};

// error handling
use crate::TofndResult;
use anyhow::anyhow;

use std::convert::TryInto;

impl Gg20Service {
    pub(super) async fn handle_recover(&self, request: proto::RecoverRequest) -> TofndResult<()> {
        // get keygen init sanitized from request
        let keygen_init = {
            let keygen_init = request
                .keygen_init
                .ok_or_else(|| anyhow!("missing keygen_init field in recovery request"))?;
            Self::keygen_sanitize_args(keygen_init)?
        };

        let keygen_output = request
            .keygen_output
            .ok_or_else(|| anyhow!("missing keygen_output field in recovery request"))?;

        // check if key-uid already exists in kv-store. If yes, return success and don't update the kv-store
        if self
            .kv_manager
            .kv()
            .exists(&keygen_init.new_key_uid)
            .await
            .map_err(|err| anyhow!(err))?
        {
            warn!(
                "Request to recover shares for [key {}, party {}] but shares already exist in kv-store. Abort request.",
                keygen_init.new_key_uid, keygen_init.party_uids[keygen_init.my_index]
            );
            return Ok(());
        }

        // recover secret key shares from request
        // get mnemonic seed
        let secret_recovery_key = self.kv_manager.seed().await?;
        let secret_key_shares = self
            .recover_secret_key_shares(&secret_recovery_key, &keygen_init, &keygen_output)
            .map_err(|err| anyhow!("Failed to acquire secret key share {}", err))?;

        Ok(self
            .update_share_kv_store(keygen_init, secret_key_shares)
            .await?)
    }

    /// get recovered secret key shares from serilized share recovery info
    fn recover_secret_key_shares(
        &self,
        secret_recovery_key: &SecretRecoveryKey,
        init: &KeygenInitSanitized,
        output: &proto::KeygenOutput,
    ) -> TofndResult<Vec<SecretKeyShare>> {
        // get my share count safely
        let my_share_count = *init.party_share_counts.get(init.my_index).ok_or_else(|| {
            anyhow!(
                "index {} is out of party_share_counts bounds {}",
                init.my_index,
                init.party_share_counts.len()
            )
        })?;
        if my_share_count == 0 {
            return Err(anyhow!("Party {} has 0 shares assigned", init.my_index));
        }

        // check party share counts
        let party_share_counts = PartyShareCounts::from_vec(init.party_share_counts.to_owned())
            .map_err(|_| {
                anyhow!(
                    "PartyCounts::from_vec() error for {:?}",
                    init.party_share_counts
                )
            })?;

        // check private recovery infos
        // use an additional layer of deserialization to simpify the protobuf definition
        // deserialize recovery info here to catch errors before spending cycles on keypair recovery
        let private_info_vec: Vec<BytesVec> = deserialize(&output.private_recover_info)
            .ok_or_else(|| anyhow!("Failed to deserialize private recovery infos"))?;

        if private_info_vec.len() != my_share_count {
            return Err(anyhow!(
                "Party {} has {} shares assigned, but retrieved {} shares from client",
                init.my_index,
                my_share_count,
                private_info_vec.len()
            ));
        }

        info!("Recovering keypair for party {} ...", init.my_index);

        let party_id = TypedUsize::<KeygenPartyId>::from_usize(init.my_index);

        // try to recover keypairs
        let session_nonce = init.new_key_uid.as_bytes();
        let party_keypair = match self.cfg.safe_keygen {
            true => recover_party_keypair(party_id, secret_recovery_key, session_nonce),
            false => recover_party_keypair_unsafe(party_id, secret_recovery_key, session_nonce),
        }
        .map_err(|_| anyhow!("party keypair recovery failed"))?;

        info!("Finished recovering keypair for party {}", init.my_index);

        // try to gather secret key shares from recovery infos
        let secret_key_shares = private_info_vec
            .iter()
            .enumerate()
            .map(|(i, share_recovery_info_bytes)| {
                SecretKeyShare::recover(
                    &party_keypair,
                    share_recovery_info_bytes, // request recovery for ith share
                    &output.group_recover_info,
                    &output.pub_key,
                    party_id,
                    i,
                    party_share_counts.clone(),
                    init.threshold,
                )
                .map_err(|_| anyhow!("Cannot recover share [{}] of party [{}]", i, party_id))
            })
            .collect::<TofndResult<_>>()?;

        Ok(secret_key_shares)
    }

    /// attempt to write recovered secret key shares to the kv-store
    async fn update_share_kv_store(
        &self,
        keygen_init_sanitized: KeygenInitSanitized,
        secret_key_shares: Vec<SecretKeyShare>,
    ) -> TofndResult<()> {
        // try to make a reservation
        let reservation = self
            .kv_manager
            .kv()
            .reserve_key(keygen_init_sanitized.new_key_uid)
            .await
            .map_err(|err| anyhow!("failed to complete reservation: {}", err))?;
        // acquire kv-data
        let kv_data = PartyInfo::get_party_info(
            secret_key_shares,
            keygen_init_sanitized.party_uids,
            keygen_init_sanitized.party_share_counts,
            keygen_init_sanitized.my_index,
        );
        // try writing the data to the kv-store
        Ok(self
            .kv_manager
            .kv()
            .put(reservation, kv_data.try_into()?)
            .await
            .map_err(|err| anyhow!("failed to update kv store: {}", err))?)
    }
}
