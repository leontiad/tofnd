// Notes:
// # Helper functions:
// Since we are using tokio, we need to make use of async function. That comes
// with the unfortunate necessity to declare some extra functions in order to
// facilitate the tests. These functions are:
// 1. src/kv_manager::KV::get_db_paths
// 2. src/gg20/mod::get_db_paths
// 3. src/gg20/mod::with_db_name

use std::convert::TryFrom;
use std::path::{Path, PathBuf};
use testdir::testdir;
use tokio::time::{sleep, Duration};
use tonic::Code::InvalidArgument;

mod mock;
mod tofnd_party;

mod honest_test_cases;
#[cfg(feature = "malicious")]
mod malicious;
#[cfg(feature = "malicious")]
use malicious::{MaliciousData, PartyMaliciousData};

mod mnemonic;

use crate::mnemonic::Cmd::{self, Create};
use proto::message_out::CriminalList;
use tracing::{info, warn};

use crate::proto::{
    self,
    message_out::{
        keygen_result::KeygenResultData::{Criminals as KeygenCriminals, Data as KeygenData},
        sign_result::SignResultData::{Criminals as SignCriminals, Signature},
        KeygenResult, SignResult,
    },
};
use mock::{Deliverer, Party};
use tofnd_party::TofndParty;

// use crate::gg20::proto_helpers::to_criminals;

lazy_static::lazy_static! {
    static ref MSG_TO_SIGN: Vec<u8> = vec![42; 32];
    // TODO add test for messages smaller and larger than 32 bytes
}
const SLEEP_TIME: u64 = 1;
const MAX_TRIES: u32 = 3;

struct TestCase {
    uid_count: usize,
    share_counts: Vec<u32>,
    threshold: usize,
    signer_indices: Vec<usize>,
    expected_keygen_faults: CriminalList,
    expected_sign_faults: CriminalList,
    #[cfg(feature = "malicious")]
    malicious_data: MaliciousData,
}

async fn run_test_cases(test_cases: &[TestCase]) {
    let restart = false;
    let recover = false;
    let dir = testdir!();
    for test_case in test_cases {
        basic_keygen_and_sign(test_case, &dir, restart, recover).await;
    }
}

async fn run_restart_test_cases(test_cases: &[TestCase]) {
    let restart = true;
    let recover = false;
    let dir = testdir!();
    for test_case in test_cases {
        basic_keygen_and_sign(test_case, &dir, restart, recover).await;
    }
}

async fn run_restart_recover_test_cases(test_cases: &[TestCase]) {
    let restart = true;
    let recover = true;
    let dir = testdir!();
    for test_case in test_cases {
        basic_keygen_and_sign(test_case, &dir, restart, recover).await;
    }
}

async fn run_keygen_fail_test_cases(test_cases: &[TestCase]) {
    let dir = testdir!();
    for test_case in test_cases {
        keygen_init_fail(test_case, &dir).await;
    }
}

async fn run_sign_fail_test_cases(test_cases: &[TestCase]) {
    let dir = testdir!();
    for test_case in test_cases {
        sign_init_fail(test_case, &dir).await;
    }
}

// Horrible code duplication indeed. Don't think we should spend time here though
// because this will be deleted when axelar-core accommodates crimes
fn successful_keygen_results(results: Vec<KeygenResult>, expected_faults: &CriminalList) -> bool {
    // get the first non-empty result. We can't simply take results[0] because some behaviours
    // don't return results and we pad them with `None`s
    let first = results.iter().find(|r| r.keygen_result_data.is_some());

    let mut pub_keys = vec![];
    for result in results.iter() {
        let res = match result.keygen_result_data.clone().unwrap() {
            KeygenData(data) => data.pub_key,
            KeygenCriminals(_) => continue,
        };
        pub_keys.push(res);
    }

    // else we have at least one result
    let first = first.unwrap().clone();
    match first.keygen_result_data {
        Some(KeygenData(data)) => {
            let first_pub_key = &data.pub_key;
            assert_eq!(
                expected_faults,
                &CriminalList::default(),
                "expected faults but none was found"
            );
            for (i, pub_key) in pub_keys.iter().enumerate() {
                assert_eq!(
                    first_pub_key, pub_key,
                    "party {} didn't produce the expected pub_key",
                    i
                );
            }
        }
        Some(KeygenCriminals(ref actual_faults)) => {
            assert_eq!(expected_faults, actual_faults);
            info!("Fault list: {:?}", expected_faults);
            return false;
        }
        None => {
            panic!("Result was None");
        }
    }
    true
}

// Horrible code duplication indeed. Don't think we should spend time here though
// because this will be deleted when axelar-core accommodates crimes
fn check_sign_results(results: Vec<SignResult>, expected_faults: &CriminalList) -> bool {
    // get the first non-empty result. We can't simply take results[0] because some behaviours
    // don't return results and we pad them with `None`s
    let first = results.iter().find(|r| r.sign_result_data.is_some());

    let mut pub_keys = vec![];
    for result in results.iter() {
        let res = match result.sign_result_data.clone().unwrap() {
            Signature(signature) => signature,
            SignCriminals(_) => continue,
        };
        pub_keys.push(res);
    }

    // else we have at least one result
    let first = first.unwrap().clone();
    match first.sign_result_data {
        Some(Signature(signature)) => {
            let first_signature = signature;
            assert_eq!(
                expected_faults,
                &CriminalList::default(),
                "expected faults but none was found"
            );
            for (i, signature) in pub_keys.iter().enumerate() {
                assert_eq!(
                    &first_signature, signature,
                    "party {} didn't produce the expected signature",
                    i
                );
            }
        }
        Some(SignCriminals(ref actual_faults)) => {
            assert_eq!(expected_faults, actual_faults);
            info!("Fault list: {:?}", expected_faults);
            return false;
        }
        None => {
            panic!("Result was None");
        }
    }
    true
}

fn gather_recover_info(results: &[KeygenResult]) -> Vec<proto::KeygenOutput> {
    // gather recover info
    let mut recover_infos = vec![];
    for result in results.iter() {
        let result_data = result.keygen_result_data.clone().unwrap();
        match result_data {
            KeygenData(output) => {
                recover_infos.push(output);
            }
            KeygenCriminals(_) => {}
        }
    }
    recover_infos
}

// shutdown i-th party
// returns i-th party's db path and a vec of Option<TofndParty> that contain all parties (including i-th)
async fn shutdown_party(
    parties: Vec<TofndParty>,
    party_index: usize,
) -> (Vec<Option<TofndParty>>, PathBuf) {
    info!("shutdown party {}", party_index);
    let party_root = parties[party_index].get_root();
    // use Option to temporarily transfer ownership of individual parties to a spawn
    let mut party_options: Vec<Option<_>> = parties.into_iter().map(Some).collect();
    let shutdown_party = party_options[party_index].take().unwrap();
    shutdown_party.shutdown().await;
    (party_options, party_root)
}

// deletes the share kv-store of a party's db path
fn delete_party_export(mut mnemonic_path: PathBuf) {
    mnemonic_path.push("export");
    std::fs::remove_file(mnemonic_path).unwrap();
}

// deletes the share kv-store of a party's db path
async fn delete_party_shares(mut party_db_path: PathBuf, key: &str) {
    party_db_path.push("kvstore/kv");
    info!("Deleting shares for {:?}", party_db_path);

    let mut tries = 0;
    let db = loop {
        match sled::open(&party_db_path) {
            Ok(db) => break db,
            Err(err) => {
                sleep(Duration::from_secs(SLEEP_TIME)).await;
                warn!("({}/{}) Cannot open db: {}", tries, err, MAX_TRIES);
            }
        }
        tries += 1;
        if tries == MAX_TRIES {
            panic!("Cannot open db");
        }
    };

    match db.remove(key) {
        Ok(_) => {}
        Err(err) => {
            panic!("Could not remove key {} from kvstore: {}", key, err)
        }
    };
}

// reinitializes i-th party
// pass malicious data if we are running in malicious mode
async fn reinit_party(
    mut party_options: Vec<Option<TofndParty>>,
    party_index: usize,
    testdir: &Path,
    #[cfg(feature = "malicious")] malicious_data: &MaliciousData,
) -> Vec<TofndParty> {
    // initialize restarted party with its previous behaviour if we are in malicious mode
    let init_party = InitParty::new(
        party_index,
        #[cfg(feature = "malicious")]
        malicious_data,
    );

    // here we assume that the party already has a mnemonic, so we pass Cmd::Existing
    party_options[party_index] = Some(TofndParty::new(init_party, Cmd::Existing, testdir).await);

    party_options
        .into_iter()
        .map(|o| o.unwrap())
        .collect::<Vec<_>>()
}

// delete all kv-stores of all parties and kill servers
async fn clean_up(parties: Vec<TofndParty>) {
    delete_dbs(&parties);
    shutdown_parties(parties).await;
}

// create parties that will participate in keygen/sign from testcase args
async fn init_parties_from_test_case(
    test_case: &TestCase,
    dir: &Path,
) -> (Vec<TofndParty>, Vec<String>) {
    let init_parties_t = InitParties::new(
        test_case.uid_count,
        #[cfg(feature = "malicious")]
        &test_case.malicious_data,
    );
    init_parties(&init_parties_t, dir).await
}

// keygen wrapper
async fn basic_keygen(
    test_case: &TestCase,
    parties: Vec<TofndParty>,
    party_uids: Vec<String>,
    new_key_uid: &str,
) -> (Vec<TofndParty>, proto::KeygenInit, Vec<KeygenResult>, bool) {
    let party_share_counts = &test_case.share_counts;
    let threshold = test_case.threshold;
    let expected_keygen_faults = &test_case.expected_keygen_faults;

    info!(
        "======= Expected keygen crimes: {:?}",
        expected_keygen_faults
    );

    #[allow(unused_variables)] // allow unsused in non malicious
    let expect_timeout = false;
    #[cfg(feature = "malicious")]
    let expect_timeout = test_case.malicious_data.keygen_data.timeout.is_some();

    let (parties, results, keygen_init) = execute_keygen(
        parties,
        &party_uids,
        party_share_counts,
        new_key_uid,
        threshold,
        expect_timeout,
    )
    .await;

    // a successful keygen does not have grpc errors
    let results = results.into_iter().map(|r| r.unwrap()).collect::<Vec<_>>();
    let success = successful_keygen_results(results.clone(), expected_keygen_faults);
    (parties, keygen_init, results, success)
}

// restart i-th and optionally delete its shares kv-store
async fn restart_party(
    dir: &Path,
    parties: Vec<TofndParty>,
    party_index: usize,
    recover: bool,
    key_uid: String,
    #[cfg(feature = "malicious")] malicious_data: &MaliciousData,
) -> Vec<TofndParty> {
    // shutdown party with party_index
    let (party_options, shutdown_db_path) = shutdown_party(parties, party_index).await;

    // if we are going to restart, delete exported mnemonic to allow using Cmd::Existing
    delete_party_export(shutdown_db_path.clone());

    if recover {
        // if we are going to recover, delete party's shares
        delete_party_shares(shutdown_db_path, &key_uid).await;
    }

    // reinit party
    let mut parties = reinit_party(
        party_options,
        party_index,
        dir,
        #[cfg(feature = "malicious")]
        malicious_data,
    )
    .await;

    if recover {
        // Check that session for the party doing recovery is absent in kvstore
        let is_key_present = parties[party_index].execute_key_presence(key_uid).await;

        assert!(
            !is_key_present,
            "Expected session to be absent after a restart"
        );
    }

    parties
}

// main testing function
async fn basic_keygen_and_sign(test_case: &TestCase, dir: &Path, restart: bool, recover: bool) {
    // set up a key uid
    let new_key_uid = "Gus-test-key";

    // use test case params to create parties
    let (parties, party_uids) = init_parties_from_test_case(test_case, dir).await;

    // Check that the session is not present in the kvstore
    let parties = execute_key_presence(parties, new_key_uid.into(), false).await;

    // execute keygen and return everything that will be needed later on
    let (parties, keygen_init, keygen_results, success) =
        basic_keygen(test_case, parties, party_uids.clone(), new_key_uid).await;

    if !success {
        clean_up(parties).await;
        return;
    }

    // Check that the session is present in the kvstore
    let parties = execute_key_presence(parties, new_key_uid.into(), true).await;

    // restart party if restart is enabled and return new parties' set
    let parties = match restart {
        true => {
            restart_party(
                dir,
                parties,
                test_case.signer_indices[0],
                recover,
                keygen_init.new_key_uid.clone(),
                #[cfg(feature = "malicious")]
                &test_case.malicious_data,
            )
            .await
        }
        false => parties,
    };

    // delete party's if recover is enabled and return new parties' set
    let parties = match recover {
        true => {
            execute_recover(
                parties,
                test_case.signer_indices[0],
                keygen_init,
                gather_recover_info(&keygen_results),
            )
            .await
        }
        false => parties,
    };

    let expected_sign_faults = &test_case.expected_sign_faults;

    #[allow(unused_variables)] // allow unsused in non malicious
    let expect_timeout = false;
    #[cfg(feature = "malicious")]
    let expect_timeout = test_case.malicious_data.sign_data.timeout.is_some();

    // Check that the session is present in the kvstore
    let parties = execute_key_presence(parties, new_key_uid.into(), true).await;

    // execute sign
    let new_sig_uid = "Gus-test-sig";
    let (parties, results) = execute_sign(
        parties,
        &party_uids,
        &test_case.signer_indices,
        new_key_uid,
        new_sig_uid,
        &MSG_TO_SIGN,
        expect_timeout,
    )
    .await;
    let results = results.into_iter().map(|r| r.unwrap()).collect::<Vec<_>>();
    check_sign_results(results, expected_sign_faults);

    clean_up(parties).await;
}

async fn keygen_init_fail(test_case: &TestCase, dir: &Path) {
    // set up a key uid
    let new_key_uid = "test-key";

    // use test case params to create parties
    let (parties, party_uids) = init_parties_from_test_case(test_case, dir).await;

    // execute keygen and return everything that will be needed later on
    let (parties, _, _, _) =
        basic_keygen(test_case, parties, party_uids.clone(), new_key_uid).await;

    // attempt to execute keygen again with the same `new_key_id`
    let (parties, results, _) = execute_keygen(
        parties,
        &party_uids,
        &test_case.share_counts,
        new_key_uid,
        test_case.threshold,
        false,
    )
    .await;

    // all results must be Err(Status) with Code::InvalidArgument
    for result in results {
        assert_eq!(result.err().unwrap().code(), InvalidArgument);
    }

    clean_up(parties).await;
}

async fn sign_init_fail(test_case: &TestCase, dir: &Path) {
    // set up a key uid
    let new_key_uid = "test-key";
    let new_sign_uid = "sign-test-key";

    // use test case params to create parties
    let (parties, party_uids) = init_parties_from_test_case(test_case, dir).await;

    // execute keygen and return everything that will be needed later on
    let (parties, _, _, success) =
        basic_keygen(test_case, parties, party_uids.clone(), new_key_uid).await;
    assert!(success);

    // attempt to execute sign with malformed `MSG_TO_SIGN`
    let (parties, results) = execute_sign(
        parties,
        &party_uids,
        &test_case.signer_indices,
        new_key_uid,
        new_sign_uid,
        &MSG_TO_SIGN[0..MSG_TO_SIGN.len() - 1],
        false,
    )
    .await;

    // all results must be Err(Status) with Code::InvalidArgument
    for result in results {
        assert_eq!(result.err().unwrap().code(), InvalidArgument);
    }

    clean_up(parties).await;
}

// struct to pass in TofndParty constructor.
// needs to include malicious when we are running in malicious mode
struct InitParty {
    party_index: usize,
    #[cfg(feature = "malicious")]
    malicious_data: PartyMaliciousData,
}

impl InitParty {
    // as ugly as it gets
    fn new(
        my_index: usize,
        #[cfg(feature = "malicious")] all_malicious_data: &MaliciousData,
    ) -> InitParty {
        #[cfg(feature = "malicious")]
        let malicious_data = {
            // register timeouts
            let mut timeout_round = 0;
            if let Some(timeout) = all_malicious_data.keygen_data.timeout.clone() {
                if timeout.index == my_index {
                    timeout_round = timeout.round;
                }
            }
            if let Some(timeout) = all_malicious_data.sign_data.timeout.clone() {
                if timeout.index == my_index {
                    timeout_round = timeout.round;
                }
            }

            // register disrupts
            let mut disrupt_round = 0;
            if let Some(disrupt) = all_malicious_data.keygen_data.disrupt.clone() {
                if disrupt.index == my_index {
                    disrupt_round = disrupt.round;
                }
            }
            if let Some(disrupt) = all_malicious_data.sign_data.disrupt.clone() {
                if disrupt.index == my_index {
                    disrupt_round = disrupt.round;
                }
            }

            // get keygen malicious behaviours
            let my_keygen_behaviour = all_malicious_data
                .keygen_data
                .behaviours
                .get(my_index)
                .unwrap()
                .clone();

            // get sign malicious behaviours
            let my_sign_behaviour = all_malicious_data
                .sign_data
                .behaviours
                .get(my_index)
                .unwrap()
                .clone();

            // construct struct of malicous data
            PartyMaliciousData {
                timeout_round,
                disrupt_round,
                keygen_behaviour: my_keygen_behaviour,
                sign_behaviour: my_sign_behaviour,
            }
        };

        InitParty {
            party_index: my_index,
            #[cfg(feature = "malicious")]
            malicious_data,
        }
    }
}

// struct to pass in init_parties function.
// needs to include malicious when we are running in malicious mode
struct InitParties {
    party_count: usize,
    #[cfg(feature = "malicious")]
    malicious_data: MaliciousData,
}

impl InitParties {
    fn new(
        party_count: usize,
        #[cfg(feature = "malicious")] malicious_data: &MaliciousData,
    ) -> InitParties {
        InitParties {
            party_count,
            #[cfg(feature = "malicious")]
            malicious_data: malicious_data.clone(),
        }
    }
}

async fn init_parties(
    init_parties: &InitParties,
    testdir: &Path,
) -> (Vec<TofndParty>, Vec<String>) {
    let mut parties = Vec::with_capacity(init_parties.party_count);

    // use a for loop because async closures are unstable https://github.com/rust-lang/rust/issues/62290
    for i in 0..init_parties.party_count {
        let init_party = InitParty::new(
            i,
            #[cfg(feature = "malicious")]
            &init_parties.malicious_data,
        );
        parties.push(TofndParty::new(init_party, Create, testdir).await);
    }

    let party_uids: Vec<String> = (0..init_parties.party_count)
        .map(|i| format!("{}", (b'A' + i as u8) as char))
        .collect();

    (parties, party_uids)
}

async fn shutdown_parties(parties: Vec<impl Party>) {
    for p in parties {
        p.shutdown().await;
    }
}

fn delete_dbs(parties: &[impl Party]) {
    for p in parties {
        // Sled creates a directory for the database and its configuration
        std::fs::remove_dir_all(p.get_root()).unwrap();
    }
}

use tonic::Status;
type GrpcKeygenResult = Result<KeygenResult, Status>;
type GrpcSignResult = Result<SignResult, Status>;

// need to take ownership of parties `parties` and return it on completion
async fn execute_keygen(
    parties: Vec<TofndParty>,
    party_uids: &[String],
    party_share_counts: &[u32],
    new_key_uid: &str,
    threshold: usize,
    expect_timeout: bool,
) -> (Vec<TofndParty>, Vec<GrpcKeygenResult>, proto::KeygenInit) {
    info!("Expecting timeout: [{}]", expect_timeout);
    let share_count = parties.len();
    let (keygen_delivery, keygen_channel_pairs) = Deliverer::with_party_ids(party_uids);
    let mut keygen_join_handles = Vec::with_capacity(share_count);
    let notify = std::sync::Arc::new(tokio::sync::Notify::new());
    for (i, (mut party, channel_pair)) in parties
        .into_iter()
        .zip(keygen_channel_pairs.into_iter())
        .enumerate()
    {
        let init = proto::KeygenInit {
            new_key_uid: new_key_uid.to_string(),
            party_uids: party_uids.to_owned(),
            party_share_counts: party_share_counts.to_owned(),
            my_party_index: u32::try_from(i).unwrap(),
            threshold: u32::try_from(threshold).unwrap(),
        };
        let delivery = keygen_delivery.clone();
        let n = notify.clone();
        let handle = tokio::spawn(async move {
            let result = party.execute_keygen(init, channel_pair, delivery, n).await;
            (party, result)
        });
        keygen_join_handles.push(handle);
    }

    // Sleep here to prevent data races between parties:
    // some clients might start sending TrafficIn messages to other parties'
    // servers before these parties manage to receive their own
    // KeygenInit/SignInit from their clients. This leads to an
    // `WrongMessage` error.
    sleep(Duration::from_secs(SLEEP_TIME)).await;
    // wake up one party
    notify.notify_one();

    // if we are expecting a timeout, abort parties after a reasonable amount of time
    if expect_timeout {
        let unblocker = keygen_delivery.clone();
        abort_parties(unblocker, 10);
    }

    let mut parties = Vec::with_capacity(share_count); // async closures are unstable https://github.com/rust-lang/rust/issues/62290
    let mut results = vec![];
    for h in keygen_join_handles {
        let handle = h.await.unwrap();
        parties.push(handle.0);
        results.push(handle.1);
    }
    let init = proto::KeygenInit {
        new_key_uid: new_key_uid.to_string(),
        party_uids: party_uids.to_owned(),
        party_share_counts: party_share_counts.to_owned(),
        my_party_index: 0, // return keygen for first party. Might need to change index before using
        threshold: u32::try_from(threshold).unwrap(),
    };
    (parties, results, init)
}

async fn execute_key_presence(
    parties: Vec<TofndParty>,
    key_uid: String,
    expected_key_present: bool,
) -> Vec<TofndParty> {
    let mut handles = Vec::new();

    for mut party in parties {
        let key_uid = key_uid.clone();

        let handle = tokio::spawn(async move {
            let res = party.execute_key_presence(key_uid).await;
            (party, res)
        });

        handles.push(handle);
    }

    let mut parties = Vec::new();

    for handle in handles {
        let (party, is_key_present) = handle.await.unwrap();
        assert_eq!(
            is_key_present, expected_key_present,
            "Key presence expected to be {} but observed {}",
            expected_key_present, is_key_present
        );

        parties.push(party);
    }

    parties
}

async fn execute_recover(
    mut parties: Vec<TofndParty>,
    recover_party_index: usize,
    mut keygen_init: proto::KeygenInit,
    keygen_outputs: Vec<proto::KeygenOutput>,
) -> Vec<TofndParty> {
    // create keygen init for recovered party
    let key_uid = keygen_init.new_key_uid.clone();

    keygen_init.my_party_index = recover_party_index as u32;
    parties[recover_party_index]
        .execute_recover(keygen_init, keygen_outputs[recover_party_index].clone())
        .await;

    // Check that session for the party doing recovery is absent in kvstore
    let is_key_present = parties[recover_party_index]
        .execute_key_presence(key_uid)
        .await;

    assert!(
        is_key_present,
        "Expected session to be present after a recovery"
    );

    parties
}

// need to take ownership of parties `parties` and return it on completion
async fn execute_sign(
    parties: Vec<TofndParty>,
    party_uids: &[String],
    sign_participant_indices: &[usize],
    key_uid: &str,
    new_sig_uid: &str,
    msg_to_sign: &[u8],
    expect_timeout: bool,
) -> (Vec<TofndParty>, Vec<GrpcSignResult>) {
    info!("Expecting timeout: [{}]", expect_timeout);
    let participant_uids: Vec<String> = sign_participant_indices
        .iter()
        .map(|&i| party_uids[i].clone())
        .collect();
    let (sign_delivery, sign_channel_pairs) = Deliverer::with_party_ids(&participant_uids);

    // use Option to temporarily transfer ownership of individual parties to a spawn
    let mut party_options: Vec<Option<_>> = parties.into_iter().map(Some).collect();

    let mut sign_join_handles = Vec::with_capacity(sign_participant_indices.len());
    let notify = std::sync::Arc::new(tokio::sync::Notify::new());
    for (i, channel_pair) in sign_channel_pairs.into_iter().enumerate() {
        let participant_index = sign_participant_indices[i];

        // clone everything needed in spawn
        let init = proto::SignInit {
            new_sig_uid: new_sig_uid.to_string(),
            key_uid: key_uid.to_string(),
            party_uids: participant_uids.clone(),
            message_to_sign: msg_to_sign.to_vec(),
        };
        let delivery = sign_delivery.clone();
        let participant_uid = participant_uids[i].clone();
        let mut party = party_options[participant_index].take().unwrap();

        let n = notify.clone();
        // execute the protocol in a spawn
        let handle = tokio::spawn(async move {
            let result = party
                .execute_sign(init, channel_pair, delivery, &participant_uid, n)
                .await;
            (party, result)
        });
        sign_join_handles.push((i, handle));
    }

    // Sleep here to prevent data races between parties:
    // some clients might start sending TrafficIn messages to other parties'
    // servers before these parties manage to receive their own
    // KeygenInit/SignInit from their clients. This leads to an
    // `WrongMessage` error.
    sleep(Duration::from_secs(SLEEP_TIME)).await;
    notify.notify_one();

    // if we are expecting a timeout, abort parties after a reasonable amount of time
    if expect_timeout {
        let unblocker = sign_delivery.clone();
        abort_parties(unblocker, 10);
    }

    let mut results = Vec::with_capacity(sign_join_handles.len());
    for (i, h) in sign_join_handles {
        info!("Running party {}", i);
        let handle = h.await.unwrap();
        party_options[sign_participant_indices[i]] = Some(handle.0);
        results.push(handle.1);
    }
    (
        party_options
            .into_iter()
            .map(|o| o.unwrap())
            .collect::<Vec<_>>(),
        results,
    )
}

fn abort_parties(unblocker: Deliverer, time: u64) {
    // send an abort message if protocol is taking too much time
    info!("I will send an abort message in {} seconds", time);
    std::thread::spawn(move || {
        unblocker.send_timeouts(time);
    });
    info!("Continuing for now");
}
