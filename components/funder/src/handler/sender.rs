use std::fmt::Debug;
use std::collections::HashMap;

use crypto::identity::PublicKey;
use crypto::crypto_rand::{RandValue, CryptoRandom};

use proto::funder::messages::{FriendTcOp, FriendMessage, RequestsStatus,
                                FunderOutgoingControl, MoveTokenRequest};
use common::canonical_serialize::CanonicalSerialize;
use identity::IdentityClient;

use super::MutableFunderHandler;

use crate::types::{FunderOutgoingComm, create_pending_request};
use crate::mutual_credit::outgoing::{QueueOperationFailure, QueueOperationError, OutgoingMc};

use crate::friend::{FriendMutation, ResponseOp, 
    ChannelStatus, SentLocalAddress, FriendState};
use crate::token_channel::{TcMutation, TcDirection, SetDirection};

use crate::freeze_guard::FreezeGuardMutation;

use crate::state::{FunderState, FunderMutation};
use crate::ephemeral::{Ephemeral, EphemeralMutation};
use crate::handler::FunderHandlerOutput;


pub struct FriendSendCommands {
    /// Try to send whatever possible through this friend.
    pub try_send: bool,
    /// Resend the outgoing move token message
    pub resend_outgoing: bool,
    /// Remote friend wants the token.
    pub wants_token: bool,
}

/*
pub enum SendMode {
    EmptyAllowed,
    EmptyNotAllowed,
}
*/

pub struct PendingMoveToken<A> {
    operations: Vec<FriendTcOp>,
    opt_local_address: Option<A>,
}

impl<A,R> MutableFunderHandler<A,R> 
where
    A: CanonicalSerialize + Clone + Debug + PartialEq + Eq + 'static,
    R: CryptoRandom,
{
    /*
    fn get_friend(&self, friend_public_key: &PublicKey) -> Option<&FriendState<A>> {
        self.state.friends.get(&friend_public_key)
    }

    /// Queue as many messages as possible into available token channel.
    fn queue_outgoing_operations(&self,
                           remote_public_key: &PublicKey,
                           outgoing_mc: &mut OutgoingMc,
                           funder_mutations: &mut Vec<FunderMutation<A>>)
                -> Result<(), QueueOperationFailure> {


        let friend = self.get_friend(remote_public_key).unwrap();

        // Set remote_max_debt if needed:
        let remote_max_debt = match &friend.channel_status {
            ChannelStatus::Consistent(token_channel) => token_channel,
            ChannelStatus::Inconsistent(_) => unreachable!(),
        }.get_remote_max_debt();


        if friend.wanted_remote_max_debt != remote_max_debt {
            outgoing_mc.queue_operation(FriendTcOp::SetRemoteMaxDebt(friend.wanted_remote_max_debt))?;
        }

        let token_channel = match &friend.channel_status {
            ChannelStatus::Consistent(token_channel) => token_channel,
            ChannelStatus::Inconsistent(_) => unreachable!(),
        };

        // Open or close requests is needed:
        let local_requests_status = &token_channel
            .get_mutual_credit()
            .state()
            .requests_status
            .local;

        if friend.wanted_local_requests_status != *local_requests_status {
            let friend_op = if let RequestsStatus::Open = friend.wanted_local_requests_status {
                FriendTcOp::EnableRequests
            } else {
                FriendTcOp::DisableRequests
            };
            outgoing_mc.queue_operation(friend_op)?;
        }

        // Send pending responses (responses and failures)
        // TODO: Possibly replace this clone with something more efficient later:
        let mut pending_responses = friend.pending_responses.clone();
        while let Some(pending_response) = pending_responses.pop_front() {
            let pending_op = match pending_response {
                ResponseOp::Response(response) => FriendTcOp::ResponseSendFunds(response),
                ResponseOp::Failure(failure) => FriendTcOp::FailureSendFunds(failure),
            };
            outgoing_mc.queue_operation(pending_op)?;
            let friend_mutation = FriendMutation::PopFrontPendingResponse;
            let funder_mutation = FunderMutation::FriendMutation((remote_public_key.clone(), friend_mutation));
            funder_mutations.push(funder_mutation);
        }

        let friend = self.get_friend(remote_public_key).unwrap();

        // Send pending requests:
        // TODO: Possibly replace this clone with something more efficient later:
        let mut pending_requests = friend.pending_requests.clone();
        while let Some(pending_request) = pending_requests.pop_front() {
            let pending_op = FriendTcOp::RequestSendFunds(pending_request);
            outgoing_mc.queue_operation(pending_op)?;
            let friend_mutation = FriendMutation::PopFrontPendingRequest;
            let funder_mutation = FunderMutation::FriendMutation((remote_public_key.clone(), friend_mutation));
            funder_mutations.push(funder_mutation);
        }

        let friend = self.get_friend(remote_public_key).unwrap();

        // Send as many pending user requests as possible:
        let mut pending_user_requests = friend.pending_user_requests.clone();
        while let Some(request_send_funds) = pending_user_requests.pop_front() {
            let request_op = FriendTcOp::RequestSendFunds(request_send_funds);
            outgoing_mc.queue_operation(request_op)?;
            let friend_mutation = FriendMutation::PopFrontPendingUserRequest;
            let funder_mutation = FunderMutation::FriendMutation((remote_public_key.clone(), friend_mutation));
            funder_mutations.push(funder_mutation);
        }
        Ok(())
    }


    async fn send_friend_move_token<'a>(&'a mut self,
                           remote_public_key: &'a PublicKey,
                           operations: Vec<FriendTcOp>,
                           opt_local_address: Option<A>,
                           funder_mutations: Vec<FunderMutation<A>>) {

        if let Some(local_address) = &opt_local_address {
            let friend = self.get_friend(remote_public_key).unwrap();

            let sent_local_address = match &friend.sent_local_address {
                SentLocalAddress::NeverSent => SentLocalAddress::LastSent(local_address.clone()),
                SentLocalAddress::Transition((_last_address, _prev_last_address)) => 
                    // We have the token, this means that there couldn't be a transition right now.
                    unreachable!(),
                SentLocalAddress::LastSent(last_address) =>
                    SentLocalAddress::Transition((local_address.clone(), last_address.clone())),
            };

            let friend_mutation = FriendMutation::SetSentLocalAddress(sent_local_address);
            let funder_mutation = FunderMutation::FriendMutation((remote_public_key.clone(), friend_mutation));
            self.apply_funder_mutation(funder_mutation);
        }

        for funder_mutation in funder_mutations {
            self.apply_funder_mutation(funder_mutation);
        }

        // Update freeze guard about outgoing requests:
        for operation in &operations {
            if let FriendTcOp::RequestSendFunds(request_send_funds) = operation {
                let pending_request = &create_pending_request(&request_send_funds);

                let freeze_guard_mutation = FreezeGuardMutation::AddFrozenCredit(
                    (pending_request.route.clone(), pending_request.dest_payment));
                let ephemeral_mutation = EphemeralMutation::FreezeGuardMutation(freeze_guard_mutation);
                self.apply_ephemeral_mutation(ephemeral_mutation);
            }
        }

        let friend = self.get_friend(remote_public_key).unwrap();

        let rand_nonce = RandValue::new(&self.rng);
        let token_channel = match &friend.channel_status {
            ChannelStatus::Consistent(token_channel) => token_channel,
            ChannelStatus::Inconsistent(_) => unreachable!(),
        };

        let tc_incoming = match token_channel.get_direction() {
            TcDirection::Outgoing(_) => unreachable!(),
            TcDirection::Incoming(tc_incoming) => tc_incoming,
        };

        let friend_move_token = await!(tc_incoming.create_friend_move_token(operations, 
                                             opt_local_address,
                                             rand_nonce,
                                             self.identity_client.clone()));

        let tc_mutation = TcMutation::SetDirection(
            SetDirection::Outgoing(friend_move_token));
        let friend_mutation = FriendMutation::TcMutation(tc_mutation);
        let funder_mutation = FunderMutation::FriendMutation((remote_public_key.clone(), friend_mutation));
        self.apply_funder_mutation(funder_mutation);

        let friend = self.get_friend(remote_public_key).unwrap();
        let token_channel = match &friend.channel_status {
            ChannelStatus::Consistent(token_channel) => token_channel,
            ChannelStatus::Inconsistent(_) => unreachable!(),
        };

        let tc_outgoing = match token_channel.get_direction() {
            TcDirection::Outgoing(tc_outgoing) => tc_outgoing,
            TcDirection::Incoming(_) => unreachable!(),
        };

        let friend_move_token_request = tc_outgoing.create_outgoing_move_token_request();

        // Add a task for sending the outgoing move token:
        self.add_outgoing_comm(FunderOutgoingComm::FriendMessage(
            (remote_public_key.clone(),
                FriendMessage::MoveTokenRequest(friend_move_token_request))));
    }


    /// Compose a large as possible message to send through the token channel to the remote side.
    /// The message should contain various operations, collected from:
    /// - Generic pending requests (Might be sent through any token channel).
    /// - Token channel specific pending responses/failures.
    /// - Commands that were initialized through AppManager.
    ///
    /// Any operations that will enter the message should be applied. For example, a failure
    /// message should cause the pending request to be removed.
    fn prepare_send<'a>(&'a self, remote_public_key: &'a PublicKey) 
        -> (Vec<FriendTcOp>, Option<A>, Vec<FunderMutation<A>>) {

        let friend = self.get_friend(remote_public_key).unwrap();
        let token_channel = match &friend.channel_status {
            ChannelStatus::Consistent(token_channel) => token_channel,
            ChannelStatus::Inconsistent(_) => unreachable!(),
        };
        let tc_incoming = match token_channel.get_direction() {
            TcDirection::Outgoing(_) => unreachable!(),
            TcDirection::Incoming(tc_incoming) => tc_incoming,
        };

        let mut outgoing_mc = tc_incoming.begin_outgoing_move_token();
        let mut funder_mutations = Vec::new();
        if let Err(queue_operation_failure) = 
                self.queue_outgoing_operations(remote_public_key, &mut outgoing_mc, &mut funder_mutations) {
            if let QueueOperationError::MaxOperationsReached = queue_operation_failure.error { 
            } else {
                unreachable!();
            }
        }
        let (operations, mc_mutations) = outgoing_mc.done();

        for mc_mutation in mc_mutations {
            let tc_mutation = TcMutation::McMutation(mc_mutation);
            let friend_mutation = FriendMutation::TcMutation(tc_mutation);
            let funder_mutation = FunderMutation::FriendMutation((remote_public_key.clone(), friend_mutation));
            funder_mutations.push(funder_mutation);
        }

        // Check if notification about local address change is required:
        let opt_local_address = match &self.state.opt_address {
            Some(local_address) => {
                let friend = self.get_friend(remote_public_key).unwrap();
                match &friend.sent_local_address {
                    SentLocalAddress::NeverSent => Some(local_address.clone()),
                    SentLocalAddress::Transition((_last_address, _prev_last_address)) => unreachable!(),
                    SentLocalAddress::LastSent(last_address) => {
                        if last_address != local_address {
                            Some(local_address.clone())
                        } else {
                            None
                        }
                    }
                }
            },
            None => None,
        };

        (operations, opt_local_address, funder_mutations)
    }


    /// Try to send whatever possible through a friend channel.
    pub async fn try_send_channel<'a>(&'a mut self,
                        remote_public_key: &'a PublicKey,
                        send_mode: SendMode) {

        let friend = self.get_friend(remote_public_key).unwrap();

        // We do not send messages if we are in an inconsistent status:
        let token_channel = match &friend.channel_status {
            ChannelStatus::Consistent(token_channel) => token_channel,
            ChannelStatus::Inconsistent(_) => return,
        };

        let may_send_empty = if let SendMode::EmptyAllowed = send_mode {true} else {false};
        let (operations, opt_local_address, funder_mutations) 
            = self.prepare_send(remote_public_key);

        // If we don't have anything to send, abort:
        if !(may_send_empty || !operations.is_empty() || opt_local_address.is_some()) {
            return;
        }

        match &token_channel.get_direction() {
            TcDirection::Incoming(_) => {
                // Send as many operations as possible to remote side:
                await!(self.send_friend_move_token(remote_public_key,
                                                   operations, 
                                                   opt_local_address,
                                                   funder_mutations));
            },
            TcDirection::Outgoing(tc_outgoing) => {
                if !tc_outgoing.token_wanted {
                    // We don't have the token. We should request it. 
                    // Mark that we have sent a request token, to make sure we don't do this again:
                    let tc_mutation = TcMutation::SetTokenWanted;
                    let friend_mutation = FriendMutation::TcMutation(tc_mutation);
                    let funder_mutation = FunderMutation::FriendMutation((remote_public_key.clone(), friend_mutation));
                    self.apply_funder_mutation(funder_mutation);
                    self.transmit_outgoing(remote_public_key);
                }
            },
        };
    }
    */

    /// Transmit the current outgoing friend_move_token.
    pub fn transmit_outgoing(&mut self,
                               remote_public_key: &PublicKey,
                               token_wanted: bool) {

        let friend = self.get_friend(remote_public_key).unwrap();
        let token_channel = match &friend.channel_status {
            ChannelStatus::Consistent(token_channel) => token_channel,
            ChannelStatus::Inconsistent(_) => unreachable!(),
        };

        let move_token = match &token_channel.get_direction() {
            TcDirection::Outgoing(tc_outgoing) => tc_outgoing.create_outgoing_move_token(),
            TcDirection::Incoming(_) => unreachable!(),
        };

        let move_token_request = MoveTokenRequest {
            friend_move_token: move_token,
            token_wanted,
        };

        // Transmit the current outgoing message:
        self.add_outgoing_comm(FunderOutgoingComm::FriendMessage(
            (remote_public_key.clone(),
                FriendMessage::MoveTokenRequest(move_token_request))));
    }

    /// Do we need to send anything to the remote side?
    pub fn estimate_pending_send(&self, 
                                 friend_public_key: &PublicKey) -> bool {
        unimplemented!();
    }


    pub async fn create_outgoing_move_token<'a>(&'a mut self, friend_public_key: &'a PublicKey,
                                   friend_send_commands: &'a FriendSendCommands, 
                                   pending_move_tokens: &'a mut HashMap<PublicKey, PendingMoveToken<A>>) {
        /*
        - Check if last sent local address is up to date.
        - Collect as many operations as possible (Not more than max ops per batch)
            1. Responses (response, failure)
            2. Pending requets
            3. User pending requests
        - When adding requests, check the following:
            - Valid by freezeguard.
            - Valid from credits point of view.
        - If a request is not valid, Pass it as a failure message to
            relevant friend.
        */

        unimplemented!();
    }

    pub async fn send_friend_iter1<'a>(&'a mut self, friend_public_key: &'a PublicKey, 
                             friend_send_commands: &'a FriendSendCommands, 
                             pending_move_tokens: &'a mut HashMap<PublicKey, PendingMoveToken<A>>) {

        if !friend_send_commands.try_send 
            && !friend_send_commands.resend_outgoing 
            && !friend_send_commands.wants_token {

            return;
        }

        let friend = match self.get_friend(&friend_public_key) {
            None => return,
            Some(friend) => friend,
        };

        let token_channel = match &friend.channel_status {
            ChannelStatus::Consistent(token_channel) => token_channel,
            ChannelStatus::Inconsistent(channel_inconsistent) => {
                if friend_send_commands.try_send || friend_send_commands.resend_outgoing {
                    self.add_outgoing_comm(FunderOutgoingComm::FriendMessage((friend_public_key.clone(),
                            FriendMessage::InconsistencyError(channel_inconsistent.local_reset_terms.clone()))));
                }
                return;
            },
        };

        if token_channel.is_outgoing() {
            if self.estimate_pending_send(friend_public_key) {
                let is_token_wanted = true;
                self.transmit_outgoing(&friend_public_key, is_token_wanted);
            } else {
                if friend_send_commands.resend_outgoing {
                    let is_token_wanted = false;
                    self.transmit_outgoing(&friend_public_key, is_token_wanted);
                }
            }
            return;
        }

        // If we are here, the token channel is incoming:

        // It will be strange if we need to resend outgoing, because the channel
        // is in incoming mode.
        assert!(!friend_send_commands.resend_outgoing);

        await!(self.create_outgoing_move_token(friend_public_key, friend_send_commands, &mut pending_move_tokens));
    }

    /// Send all possible messages according to SendCommands
    pub async fn send(&mut self) -> FunderHandlerOutput<A> {
        /*
        - Keep state of remote side's `token_wanted` (token_channel.rs?)
        - try_send_channel should run only once, after handling a FunderIncoming
            message.
            - Iterater over all (marked?) friends:
                - Outgoing friend
                    - If we have anything to send:
                        - send a token wanted message.
                - Incoming message
                    - First iteration:
                        - Check if last sent local address is up to date.
                        - Collect as many operations as possible (Not more than max ops per batch)
                            1. Responses (response, failure)
                            2. Pending requets
                            3. User pending requests
                        - When adding requests, check the following:
                            - Valid by freezeguard.
                            - Valid from credits point of view.
                        - If a request is not valid, Pass it as a failure message to
                            relevant friend.
                    - Second iteration: Attempt to queue newly created failure messages to all messages.
                    - Send created messages. If we have anything more to send, add a
                        `token_wanted` marker.
        */

        let pending_move_tokens: HashMap<PublicKey, PendingMoveToken<A>> 
            = HashMap::new();

        // First iteration:
        for (friend_public_key, friend_send_commands) in self.send_commands {
            await!(self.send_friend_iter1(&friend_public_key,
                                          &friend_send_commands,
                                          &mut pending_move_tokens);
        }

        // Second iteration (Attempt to queue failures created in the first iteration):
        for (friend_public_key, pending_move_token) in &mut pending_move_tokens {
            await!(self.append_failures_to_move_token(friend_public_key, pending_move_token))
        }

        self.send_move_tokens(pending_move_tokens);
    }
}



