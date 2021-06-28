use super::{mock::SenderReceiver, Deliverer, InitParty, Party};
use crate::{
    addr,
    gg20::{self, mnemonic::Cmd},
    proto,
};
use proto::message_out::{KeygenResult, SignResult};
use std::convert::TryFrom;
use std::path::Path;
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};
use tonic::Request;

#[cfg(feature = "malicious")]
use super::malicious::{
    KeygenMsgMeta, KeygenSpoof, MsgType, PartyMaliciousData, SignMsgMeta, SignSpoof, Spoof::*,
};

// I tried to keep this struct private and return `impl Party` from new() but ran into so many problems with the Rust compiler
// I also tried using Box<dyn Party> but ran into this: https://github.com/rust-lang/rust/issues/63033
pub(super) struct TofndParty {
    db_name: String,
    client: proto::gg20_client::Gg20Client<tonic::transport::Channel>,
    server_handle: JoinHandle<()>,
    server_shutdown_sender: oneshot::Sender<()>,
    server_port: u16,
    #[cfg(feature = "malicious")]
    pub(super) malicious_data: PartyMaliciousData,
}

impl TofndParty {
    // we have to have different functions for keygen and sign because sign messages can be
    // desirialized as keygen messages and vice versa. So we call the approriate function for each phase.
    #[cfg(feature = "malicious")]
    pub(crate) fn should_timeout_keygen(&self, traffic: &proto::TrafficOut) -> bool {
        let payload = traffic.clone().payload;

        // this would also work!!!
        // let msg_type: MsgType = bincode::deserialize(&payload).unwrap();

        // check if we need to stall keygen msg
        if let Ok(msg_meta) = bincode::deserialize::<KeygenMsgMeta>(&payload) {
            let keygen_msg_type = &msg_meta.msg_type;
            if let Some(timeout) = &self.malicious_data.timeout {
                let in_msg = MsgType::KeygenMsgType {
                    msg_type: keygen_msg_type.clone(),
                };
                if timeout.msg_type == in_msg {
                    println!("I am stalling keygen message {:?}", keygen_msg_type);
                    return true;
                }
            }
        }
        false
    }

    #[cfg(feature = "malicious")]
    pub(crate) fn should_timeout_sign(&self, traffic: &proto::TrafficOut) -> bool {
        let payload = traffic.clone().payload;

        // this would also work!!!
        // let msg_type: MsgType = bincode::deserialize(&payload).unwrap();

        // check if we need to stall sign msg
        if let Ok(msg_meta) = bincode::deserialize::<SignMsgMeta>(&payload) {
            let sign_msg_type = &msg_meta.msg_type;
            if let Some(timeout) = &self.malicious_data.timeout {
                let in_msg = MsgType::SignMsgType {
                    msg_type: sign_msg_type.clone(),
                };
                if timeout.msg_type == in_msg {
                    println!("I am stalling sign message {:?}", sign_msg_type);
                    return true;
                }
            }
        }
        false
    }

    // we have to have different functions for keygen and sign because sign messages can be
    // desirialized as keygen messages and vice versa. So we call the approriate function for each phase.
    #[cfg(feature = "malicious")]
    pub(crate) fn disrupt_keygen(&self, traffic: &proto::TrafficOut) -> Option<proto::TrafficOut> {
        let payload = traffic.clone().payload;
        let msg_meta: KeygenMsgMeta = bincode::deserialize(&payload).unwrap();

        // this also works!!!
        // let msg_type: MsgType = bincode::deserialize(&payload).unwrap();

        // wrap incoming msg_type into our general MsgType enum
        let msg_type = MsgType::KeygenMsgType {
            msg_type: msg_meta.msg_type,
        };

        // if I am not disrupting, return none. I dislike that I have to clone this
        let disrupt = self.malicious_data.disrupt.clone()?;
        if disrupt.msg_type != msg_type {
            return None;
        }
        println!("I am disrupting keygen message {:?}", msg_type);

        let mut disrupt_traffic = traffic.clone();
        disrupt_traffic.payload = payload[0..payload.len() / 2].to_vec();

        Some(disrupt_traffic)
    }

    #[cfg(feature = "malicious")]
    pub(crate) fn disrupt_sign(&self, traffic: &proto::TrafficOut) -> Option<proto::TrafficOut> {
        let payload = traffic.clone().payload;
        let msg_meta: SignMsgMeta = bincode::deserialize(&payload).unwrap();

        // this also works!!!
        // let msg_type: MsgType = bincode::deserialize(&payload).unwrap();

        // wrap incoming msg_type into our general MsgType enum
        let msg_type = MsgType::SignMsgType {
            msg_type: msg_meta.msg_type,
        };

        // if I am not disrupting, return none. I dislike that I have to clone this
        let disrupt = self.malicious_data.disrupt.clone()?;
        if disrupt.msg_type != msg_type {
            return None;
        }
        println!("I am disrupting sign message {:?}", msg_type);

        let mut disrupt_traffic = traffic.clone();
        disrupt_traffic.payload = payload[0..payload.len() / 2].to_vec();

        Some(disrupt_traffic)
    }

    // we have to have different functions for keygen and sign because sign messages can be
    // desirialized as keygen messages and vice versa. So we call the approriate function for each phase.
    #[cfg(feature = "malicious")]
    pub(crate) fn spoof_keygen(
        &mut self,
        traffic: &proto::TrafficOut,
    ) -> Option<proto::TrafficOut> {
        let payload = traffic.clone().payload;
        let mut msg_meta: KeygenMsgMeta = bincode::deserialize(&payload).unwrap();

        // this also works!!!
        // let msg_type: MsgType = bincode::deserialize(&payload).unwrap();

        let msg_type = &msg_meta.msg_type;

        // if I am not a spoofer, return none. I dislike that I have to clone this
        let spoof = self.malicious_data.spoof.clone()?;
        if let KeygenSpoofType { spoof } = spoof {
            if KeygenSpoof::msg_to_status(msg_type) != spoof.status {
                return None;
            }
            println!(
                "I am spoofing keygen message {:?}. Changing from [{}] -> [{}]",
                msg_type, msg_meta.from, spoof.victim
            );
            msg_meta.from = spoof.victim;
        }

        let mut spoofed_traffic = traffic.clone();
        let spoofed_payload = bincode::serialize(&msg_meta).unwrap();
        spoofed_traffic.payload = spoofed_payload;

        Some(spoofed_traffic)
    }

    #[cfg(feature = "malicious")]
    pub(crate) fn spoof_sign(&mut self, traffic: &proto::TrafficOut) -> Option<proto::TrafficOut> {
        let payload = traffic.clone().payload;
        let mut msg_meta: SignMsgMeta = bincode::deserialize(&payload).unwrap();

        // this also works!!!
        // let msg_type: MsgType = bincode::deserialize(&payload).unwrap();

        let msg_type = &msg_meta.msg_type;

        // if I am not a spoofer, return none. I dislike that I have to clone this
        let spoof = self.malicious_data.spoof.clone()?;
        if let SignSpoofType { spoof } = spoof {
            if SignSpoof::msg_to_status(msg_type) != spoof.status {
                return None;
            }
            println!(
                "I am spoofing sign message {:?}. Changing from [{}] -> [{}]",
                msg_type, msg_meta.from, spoof.victim
            );
            msg_meta.from = spoof.victim;
        }

        let mut spoofed_traffic = traffic.clone();
        let spoofed_payload = bincode::serialize(&msg_meta).unwrap();
        spoofed_traffic.payload = spoofed_payload;

        Some(spoofed_traffic)
    }

    pub(super) async fn new(init_party: InitParty, mnemonic_cmd: Cmd, testdir: &Path) -> Self {
        let db_name = format!("test-key-{:02}", init_party.party_index);
        let db_path = testdir.join(db_name);
        let db_path = db_path.to_str().unwrap();

        // start server
        let (server_shutdown_sender, shutdown_receiver) = oneshot::channel::<()>();

        // start service with respect to the current build
        #[cfg(not(feature = "malicious"))]
        let my_service = gg20::tests::with_db_name(&db_path, mnemonic_cmd).await;
        #[cfg(feature = "malicious")]
        let my_service = gg20::tests::with_db_name_malicious(
            &db_path,
            mnemonic_cmd,
            init_party.malicious_data.keygen_behaviour.clone(),
            init_party.malicious_data.sign_behaviour.clone(),
        )
        .await;

        let proto_service = proto::gg20_server::Gg20Server::new(my_service);
        let incoming = TcpListener::bind(addr(0)).await.unwrap(); // use port 0 and let the OS decide
        let server_addr = incoming.local_addr().unwrap();
        let server_port = server_addr.port();
        println!("new party bound to port [{:?}]", server_port);
        // let (startup_sender, startup_receiver) = tokio::sync::oneshot::channel::<()>();
        let server_handle = tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(proto_service)
                .serve_with_incoming_shutdown(incoming, async {
                    shutdown_receiver.await.unwrap();
                })
                .await
                .unwrap();
            // startup_sender.send(()).unwrap();
        });

        // TODO get the server to notify us after it's started, or perhaps just "yield" here
        // println!(
        //     "new party [{}] TODO sleep waiting for server to start...",
        //     server_port
        // );
        // tokio::time::delay_for(std::time::Duration::from_millis(100)).await;
        // startup_receiver.await.unwrap();
        // println!("party [{}] server started!", init.party_uids[my_id_index]);

        println!("new party [{}] connect to server...", server_port);
        let client = proto::gg20_client::Gg20Client::connect(format!("http://{}", server_addr))
            .await
            .unwrap();

        TofndParty {
            db_name: db_path.to_owned(),
            client,
            server_handle,
            server_shutdown_sender,
            server_port,
            #[cfg(feature = "malicious")]
            malicious_data: init_party.malicious_data,
        }
    }
}

#[tonic::async_trait]
impl Party for TofndParty {
    async fn execute_keygen(
        &mut self,
        init: proto::KeygenInit,
        channels: SenderReceiver,
        mut delivery: Deliverer,
    ) -> KeygenResult {
        let my_uid = init.party_uids[usize::try_from(init.my_party_index).unwrap()].clone();
        let my_display_name = format!("{}:{}", my_uid, self.server_port); // uid:port
        let (keygen_server_incoming, rx) = channels;
        let mut keygen_server_outgoing = self
            .client
            .keygen(Request::new(rx))
            .await
            .unwrap()
            .into_inner();

        // the first outbound message is keygen init info
        keygen_server_incoming
            .send(proto::MessageIn {
                data: Some(proto::message_in::Data::KeygenInit(init)),
            })
            .unwrap();

        let mut result: Option<KeygenResult> = None;
        while let Some(msg) = keygen_server_outgoing.message().await.unwrap() {
            let msg_type = msg.data.as_ref().expect("missing data");

            match msg_type {
                #[cfg(not(feature = "malicious"))]
                proto::message_out::Data::Traffic(_) => {
                    delivery.deliver(&msg, &my_uid);
                }
                // in malicous case, if we are stallers we skip the message
                #[cfg(feature = "malicious")]
                proto::message_out::Data::Traffic(traffic) => {
                    // check if I am not a staller, send the message. This is for timeout tests
                    if !self.should_timeout_keygen(&traffic) {
                        // if I am disrupting, create a _duplicate_ message and disrupt it. This is for disrupt tests
                        if let Some(traffic) = self.disrupt_keygen(&traffic) {
                            let mut disrupt_msg = msg.clone();
                            disrupt_msg.data = Some(proto::message_out::Data::Traffic(traffic));
                            delivery.deliver(&disrupt_msg, &my_uid);
                        }
                        // if I am a spoofer, create a _duplicate_ message and spoof it. This is for spoof tests
                        if let Some(traffic) = self.spoof_keygen(&traffic) {
                            let mut spoofed_msg = msg.clone();
                            spoofed_msg.data = Some(proto::message_out::Data::Traffic(traffic));
                            delivery.deliver(&spoofed_msg, &my_uid);
                        }
                        // finally, act normally and send the correct message
                        delivery.deliver(&msg, &my_uid);
                    }
                }
                proto::message_out::Data::KeygenResult(res) => {
                    result = Some(res.clone());
                    println!("party [{}] keygen finished!", my_display_name);
                    break;
                }
                _ => panic!(
                    "party [{}] keygen errpr: bad outgoing message type",
                    my_display_name
                ),
            };
        }

        if result.is_none() {
            println!(
                "party [{}] keygen execution was not completed",
                my_display_name
            );
            return KeygenResult::default();
        }

        println!("party [{}] keygen execution complete", my_display_name);

        result.unwrap()
    }

    async fn execute_recover(
        &mut self,
        keygen_init: proto::KeygenInit,
        share_recovery_infos: Vec<Vec<u8>>,
    ) {
        let keygen_init = Some(keygen_init);
        let recover_request = proto::RecoverRequest {
            keygen_init,
            share_recovery_infos,
        };
        let response = self
            .client
            .recover(Request::new(recover_request))
            .await
            .unwrap()
            .into_inner();

        // prost way to convert i32 to enums https://github.com/danburkert/prost#enumerations
        match proto::recover_response::Response::from_i32(response.response) {
            Some(proto::recover_response::Response::Success) => {
                println!("Got success from recover")
            }
            Some(proto::recover_response::Response::Fail) => {
                println!("Got fail from recover")
            }
            None => {
                panic!("Invalid recovery response. Could not convert i32 to enum")
            }
        }
    }

    async fn execute_sign(
        &mut self,
        init: proto::SignInit,
        channels: SenderReceiver,
        mut delivery: Deliverer,
        my_uid: &str,
    ) -> SignResult {
        let my_display_name = format!("{}:{}", my_uid, self.server_port); // uid:port
        let (sign_server_incoming, rx) = channels;
        let mut sign_server_outgoing = self
            .client
            .sign(Request::new(rx))
            .await
            .unwrap()
            .into_inner();

        // the first outbound message is sign init info
        sign_server_incoming
            .send(proto::MessageIn {
                data: Some(proto::message_in::Data::SignInit(init)),
            })
            .unwrap();

        // use Option of SignResult to avoid giving a default value to SignResult
        let mut result: Option<SignResult> = None;
        while let Some(msg) = sign_server_outgoing.message().await.unwrap() {
            let msg_type = msg.data.as_ref().expect("missing data");

            match msg_type {
                // in honest case, we always send the message
                #[cfg(not(feature = "malicious"))]
                proto::message_out::Data::Traffic(_) => {
                    delivery.deliver(&msg, &my_uid);
                }
                // in malicous case, if we are stallers we skip the message
                #[cfg(feature = "malicious")]
                proto::message_out::Data::Traffic(traffic) => {
                    // check if I am not a staller, send the message. This is for timeout tests
                    if !self.should_timeout_sign(&traffic) {
                        // if I am disrupting, create a _duplicate_ message and disrupt it. This is for disrupt tests
                        if let Some(traffic) = self.disrupt_sign(&traffic) {
                            let mut disrupt_msg = msg.clone();
                            disrupt_msg.data = Some(proto::message_out::Data::Traffic(traffic));
                            delivery.deliver(&disrupt_msg, &my_uid);
                        }
                        // if I am a spoofer, create a _duplicate_ message and spoof it. This is for spoof tests
                        if let Some(traffic) = self.spoof_sign(&traffic) {
                            let mut spoofed_msg = msg.clone();
                            spoofed_msg.data = Some(proto::message_out::Data::Traffic(traffic));
                            delivery.deliver(&spoofed_msg, &my_uid);
                        }
                        // finally, act normally and send the correct message
                        delivery.deliver(&msg, &my_uid);
                    }
                }
                proto::message_out::Data::SignResult(res) => {
                    result = Some(res.clone());
                    println!("party [{}] sign finished!", my_display_name);
                    break;
                }
                proto::message_out::Data::NeedRecover(res) => {
                    println!(
                        "party [{}] needs recover for session [{}]",
                        my_display_name, res.session_id
                    );
                    // when recovery is needed, sign is canceled. We abort the protocol manualy instead of waiting parties to time out
                    // no worries that we don't wait for enough time, we will not be checking criminals in this case
                    delivery.send_timeouts(0);
                    break;
                }
                _ => panic!(
                    "party [{}] sign error: bad outgoing message type",
                    my_display_name
                ),
            };
        }

        // return default value for SignResult if socket closed before I received the result
        if result.is_none() {
            println!(
                "party [{}] sign execution was not completed",
                my_display_name
            );
            return SignResult::default();
        }
        println!("party [{}] sign execution complete", my_display_name);

        result.unwrap() // it's safe to unwrap here
    }

    async fn shutdown(mut self) {
        self.server_shutdown_sender.send(()).unwrap(); // tell the server to shut down
        self.server_handle.await.unwrap(); // wait for server to shut down
        println!("party [{}] shutdown success", self.server_port);
    }

    fn get_db_path(&self) -> std::path::PathBuf {
        gg20::tests::get_db_path(&self.db_name)
    }
}
