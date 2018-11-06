#![warn(unused)]

use std::convert::TryFrom;
use byteorder::{BigEndian, WriteBytesExt};

use crypto::identity::{PublicKey, Signature, PUBLIC_KEY_LEN, SIGNATURE_LEN};
use crypto::crypto_rand::{RandValue, RAND_VALUE_LEN};
use crypto::hash::sha_512_256;
use identity::IdentityClient;

use crate::consts::MAX_OPERATIONS_IN_BATCH;

use crate::mutual_credit::types::{MutualCredit, McMutation};
use crate::mutual_credit::incoming::{ProcessOperationOutput, ProcessTransListError, 
    process_operations_list, IncomingMessage};
use crate::mutual_credit::outgoing::OutgoingMc;

use crate::types::{FriendMoveToken, 
    FriendMoveTokenRequest, ResetTerms, FriendTcOp};


// Prefix used for chain hashing of token channel funds.
// NEXT is used for hashing for the next move token funds.
// RESET is used for resetting the token channel.
// The prefix allows the receiver to distinguish between the two cases.
// const TOKEN_NEXT: &[u8] = b"NEXT";
const TOKEN_RESET: &[u8] = b"RESET";

#[derive(Clone, Serialize, Deserialize)]
pub struct OutgoingMoveToken {
    pub outgoing_move_token_request: FriendMoveTokenRequest,
    pub opt_prev_incoming_move_token: Option<FriendMoveToken>,
}


/// Indicate the direction of the move token funds.
#[derive(Clone, Serialize, Deserialize)]
pub enum MoveTokenDirection {
    Incoming(FriendMoveToken),
    Outgoing(OutgoingMoveToken),
}

pub enum SetDirection {
    Incoming(FriendMoveToken), 
    Outgoing(FriendMoveToken),
}

#[allow(unused)]
pub enum TcMutation {
    McMutation(McMutation),
    SetDirection(SetDirection),
    SetTokenWanted,
}


#[derive(Clone, Serialize, Deserialize)]
pub struct TokenChannel {
    pub direction: MoveTokenDirection,
    pub mutual_credit: MutualCredit,
}

#[derive(Debug)]
pub enum ReceiveMoveTokenError {
    ChainInconsistency,
    InvalidTransaction(ProcessTransListError),
    InvalidSignature,
    InvalidStatedBalance,
    InvalidInconsistencyCounter,
    MoveTokenCounterOverflow,
    InvalidMoveTokenCounter,
}

pub struct MoveTokenReceived {
    pub incoming_messages: Vec<IncomingMessage>,
    pub mutations: Vec<TcMutation>,
}


pub enum ReceiveMoveTokenOutput {
    Duplicate,
    RetransmitOutgoing(FriendMoveToken),
    Received(MoveTokenReceived),
    // In case of a reset, all the local pending requests will be canceled.
}



/// Calculate the token to be used for resetting the channel.
#[allow(unused)]
pub async fn calc_channel_reset_token(new_token: &Signature,
                      balance_for_reset: i128,
                      identity_client: IdentityClient) -> Signature {

    let mut sig_buffer = Vec::new();
    sig_buffer.extend_from_slice(&sha_512_256(TOKEN_RESET));
    sig_buffer.extend_from_slice(&new_token);
    sig_buffer.write_i128::<BigEndian>(balance_for_reset).unwrap();
    await!(identity_client.request_signature(sig_buffer)).unwrap()
}

/// Create a token from a public key
/// Currently this function puts the public key in the beginning of the signature buffer,
/// as the public key is shorter than a signature.
/// Possibly change this in the future (Maybe use a hash to spread the public key over all the
/// bytes of the signature)
///
/// Note that the output here is not a real signature. This function is used for the first
/// deterministic initialization of a token channel.
fn token_from_public_key(public_key: &PublicKey) -> Signature {
    let mut buff = [0; SIGNATURE_LEN];
    buff[0 .. PUBLIC_KEY_LEN].copy_from_slice(public_key);
    Signature::from(buff)
}

/// Generate a random nonce from public key.
/// Note that the result here is not really a random nonce. This function is used for the first
/// deterministic initialization of a token channel.
fn rand_nonce_from_public_key(public_key: &PublicKey) -> RandValue {
    let public_key_hash = sha_512_256(public_key);
    RandValue::try_from(&public_key_hash.as_ref()[.. RAND_VALUE_LEN]).unwrap()
}

impl TokenChannel {
    pub fn new(local_public_key: &PublicKey, 
               remote_public_key: &PublicKey) -> TokenChannel {

        let balance = 0;
        let mutual_credit = MutualCredit::new(&local_public_key, &remote_public_key, balance);
        let rand_nonce = rand_nonce_from_public_key(&remote_public_key);

        // This is a special initialization case.
        // Note that this is the only case where new_token is not a valid signature.
        // We do this because we want to have synchronization between the two sides of the token
        // channel, however, the remote side has no means of generating the signature (Because he
        // doesn't have the private key). Therefore we use a dummy new_token instead.
        let first_move_token_lower = FriendMoveToken {
            operations: Vec::new(),
            old_token: token_from_public_key(&local_public_key),
            inconsistency_counter: 0, 
            move_token_counter: 0,
            balance: 0,
            local_pending_debt: 0,
            remote_pending_debt: 0,
            rand_nonce,
            new_token: token_from_public_key(&remote_public_key),
        };

        if sha_512_256(&local_public_key) < sha_512_256(&remote_public_key) {
            // We are the first sender
            let friend_move_token_request = FriendMoveTokenRequest {
                friend_move_token: first_move_token_lower.clone(),
                token_wanted: false,
            };
            let outgoing_move_token = OutgoingMoveToken {
                outgoing_move_token_request: friend_move_token_request,
                opt_prev_incoming_move_token: None,
            };
            TokenChannel {
                direction: MoveTokenDirection::Outgoing(outgoing_move_token),
                mutual_credit,
            }
        } else {
            // We are the second sender
            TokenChannel {
                direction: MoveTokenDirection::Incoming(first_move_token_lower),
                mutual_credit,
            }
        }
    }

    pub fn new_from_remote_reset(local_public_key: &PublicKey, 
                      remote_public_key: &PublicKey, 
                      reset_move_token: &FriendMoveToken,
                      balance: i128) -> TokenChannel {

        TokenChannel {
            direction: MoveTokenDirection::Incoming(reset_move_token.clone()),
            mutual_credit: MutualCredit::new(local_public_key, remote_public_key, balance),
        }
    }

    pub fn new_from_local_reset(local_public_key: &PublicKey, 
                      remote_public_key: &PublicKey, 
                      reset_move_token: &FriendMoveToken,
                      balance: i128,
                      opt_last_incoming_move_token: Option<FriendMoveToken>) -> TokenChannel {

        let friend_move_token_request = FriendMoveTokenRequest {
            friend_move_token: reset_move_token.clone(),
            token_wanted: false,
        };
        let outgoing_move_token = OutgoingMoveToken {
            outgoing_move_token_request: friend_move_token_request,
            opt_prev_incoming_move_token: opt_last_incoming_move_token,
        };
        TokenChannel {
            direction: MoveTokenDirection::Outgoing(outgoing_move_token),
            mutual_credit: MutualCredit::new(local_public_key, remote_public_key, balance),
        }
    }

    pub async fn create_friend_move_token(&self,
                                    operations: Vec<FriendTcOp>,
                                    rand_nonce: RandValue,
                                    identity_client: IdentityClient) -> Option<FriendMoveToken> {

        let friend_move_token = match &self.direction {
            MoveTokenDirection::Incoming(friend_move_token) => friend_move_token,
            MoveTokenDirection::Outgoing(_) => return None,
        };

        Some(await!(FriendMoveToken::new(
            operations,
            friend_move_token.new_token.clone(),
            friend_move_token.inconsistency_counter,
            friend_move_token.move_token_counter.wrapping_add(1),
            self.get_mutual_credit().state().balance.balance,
            self.get_mutual_credit().state().balance.local_pending_debt,
            self.get_mutual_credit().state().balance.remote_pending_debt,
            rand_nonce,
            identity_client)))
    }


    fn get_cur_move_token(&self) -> &FriendMoveToken {
        match &self.direction {
            MoveTokenDirection::Incoming(friend_move_token) => friend_move_token,
            MoveTokenDirection::Outgoing(outgoing_move_token) => 
                &outgoing_move_token
                    .outgoing_move_token_request
                    .friend_move_token,
        }
    }

    /// Get the last incoming move token
    /// If no such incoming move token exists (Maybe this is the beginning of the relationship),
    /// returns None.
    pub fn get_last_incoming_move_token(&self) -> Option<&FriendMoveToken> {
        match &self.direction {
            MoveTokenDirection::Incoming(friend_move_token) => Some(friend_move_token),
            MoveTokenDirection::Outgoing(outgoing_move_token) => {
                match &outgoing_move_token.opt_prev_incoming_move_token {
                    None => None,
                    Some(prev_incoming_move_token) => Some(prev_incoming_move_token),
                }
            },
        }
    }

    /// Get a reference to internal mutual_credit.
    pub fn get_mutual_credit(&self) -> &MutualCredit {
        &self.mutual_credit
    }

    pub fn remote_max_debt(&self) -> u128 {
        self.get_mutual_credit().state().balance.remote_max_debt
    }

    pub fn get_inconsistency_counter(&self) -> u64 {
        self.get_cur_move_token().inconsistency_counter
    }

    pub fn get_move_token_counter(&self) -> u128 {
        self.get_cur_move_token().move_token_counter
    }


    pub async fn get_reset_terms(&self, identity_client: IdentityClient) -> ResetTerms {
        // We add 2 for the new counter in case 
        // the remote side has already used the next counter.
        let reset_token = await!(calc_channel_reset_token(
                                &self.get_cur_move_token().new_token,
                                 self.get_mutual_credit().balance_for_reset(),
                                 identity_client));
        ResetTerms {
            reset_token,
            // TODO: Should we do something other than wrapping_add(1)?
            // 2**64 inconsistencies are required for an overflow.
            inconsistency_counter: self.get_inconsistency_counter().wrapping_add(1),
            balance_for_reset: self.get_mutual_credit().balance_for_reset(),
        }
    }

    pub fn is_outgoing(&self) -> bool {
        match self.direction {
            MoveTokenDirection::Incoming(_) => false,
            MoveTokenDirection::Outgoing(_) => true,
        }
    }

    pub fn mutate(&mut self, d_mutation: &TcMutation) {
        match d_mutation {
            TcMutation::McMutation(mc_mutation) => {
                self.mutual_credit.mutate(mc_mutation);
            },
            TcMutation::SetDirection(ref set_direction) => {
                self.direction = match set_direction {
                    SetDirection::Incoming(new_token) => MoveTokenDirection::Incoming(new_token.clone()),
                    SetDirection::Outgoing(friend_move_token) => {
                        let friend_move_token_request = FriendMoveTokenRequest {
                            friend_move_token: friend_move_token.clone(),
                            token_wanted: false,
                        };
                        let outgoing_move_token = OutgoingMoveToken {
                            outgoing_move_token_request: friend_move_token_request,
                            opt_prev_incoming_move_token: self.get_last_incoming_move_token().cloned(),
                        };
                        MoveTokenDirection::Outgoing(outgoing_move_token)
                    }
                };
            },
            TcMutation::SetTokenWanted => {
                match self.direction {
                    MoveTokenDirection::Incoming(_) => unreachable!(),
                    MoveTokenDirection::Outgoing(ref mut outgoing_move_token) => {
                        outgoing_move_token.outgoing_move_token_request.token_wanted = true;
                    },
                }
            },
        }
    }

    /// Deal with the case of incoming state turning into outgoing state.
    fn outgoing_to_incoming(&self, 
                            old_move_token: &FriendMoveToken, 
                            new_move_token: FriendMoveToken) 
        -> Result<ReceiveMoveTokenOutput, ReceiveMoveTokenError> {
    
        // Verify counters:
        if new_move_token.inconsistency_counter != old_move_token.inconsistency_counter {
            return Err(ReceiveMoveTokenError::InvalidInconsistencyCounter);
        }

        let expected_move_token_counter = old_move_token.move_token_counter
            .checked_add(1)
            .ok_or(ReceiveMoveTokenError::MoveTokenCounterOverflow)?;

        if new_move_token.move_token_counter != expected_move_token_counter {
            return Err(ReceiveMoveTokenError::InvalidMoveTokenCounter);
        }

        let mut mutual_credit = self.mutual_credit.clone();
        let res = process_operations_list(&mut mutual_credit,
            new_move_token.operations.clone());

        // Verify balance:
        if mutual_credit.state().balance.balance != new_move_token.balance ||
           mutual_credit.state().balance.local_pending_debt != new_move_token.local_pending_debt ||
           mutual_credit.state().balance.remote_pending_debt != new_move_token.remote_pending_debt {
            return Err(ReceiveMoveTokenError::InvalidStatedBalance);
        }

        match res {
            Ok(outputs) => {
                let mut move_token_received = MoveTokenReceived {
                    incoming_messages: Vec::new(),
                    mutations: Vec::new(),
                };

                for output in outputs {
                    let ProcessOperationOutput 
                        {incoming_message, mc_mutations} = output;

                    if let Some(funds) = incoming_message {
                        move_token_received.incoming_messages.push(funds);
                    }
                    for mc_mutation in mc_mutations {
                        move_token_received.mutations.push(
                            TcMutation::McMutation(mc_mutation));
                    }
                }
                move_token_received.mutations.push(
                    TcMutation::SetDirection(SetDirection::Incoming(new_move_token)));
                Ok(ReceiveMoveTokenOutput::Received(move_token_received))
            },
            Err(e) => {
                Err(ReceiveMoveTokenError::InvalidTransaction(e))
            },
        }
    }


    #[allow(unused)]
    pub fn simulate_receive_move_token(&self, 
                              new_move_token: FriendMoveToken)
        -> Result<ReceiveMoveTokenOutput, ReceiveMoveTokenError> {

        match &self.direction {
            MoveTokenDirection::Incoming(friend_move_token) => {
                if friend_move_token == &new_move_token {
                    // Duplicate
                    Ok(ReceiveMoveTokenOutput::Duplicate)
                } else {
                    // Inconsistency
                    Err(ReceiveMoveTokenError::ChainInconsistency)
                }
            },
            MoveTokenDirection::Outgoing(ref outgoing_move_token) => {
                // Verify signature:
                // Note that we only verify the signature here, and not at the Incoming part.
                // This allows the genesis move token to occur smoothly, even though its signature
                // is not correct.
                let remote_public_key = &self.get_mutual_credit().state().idents.remote_public_key;
                if !new_move_token.verify(remote_public_key) {
                    return Err(ReceiveMoveTokenError::InvalidSignature);
                }


                let friend_move_token = &outgoing_move_token.outgoing_move_token_request.friend_move_token;
                if &new_move_token.old_token == &self.get_cur_move_token().new_token {
                    self.outgoing_to_incoming(friend_move_token, new_move_token)
                } else if friend_move_token.old_token == new_move_token.new_token {
                    // We should retransmit our move token message to the remote side.
                    Ok(ReceiveMoveTokenOutput::RetransmitOutgoing(friend_move_token.clone()))
                } else {
                    Err(ReceiveMoveTokenError::ChainInconsistency)
                }
            },
        }
    }

    pub fn begin_outgoing_move_token(&self) -> Option<OutgoingMc> {
        if let MoveTokenDirection::Outgoing(_) = self.direction {
            return None;
        }

        Some(OutgoingMc::new(&self.mutual_credit, MAX_OPERATIONS_IN_BATCH))
    }


    /// Get the current outgoing move token
    /// If current state is not outgoing, returns None
    pub fn get_outgoing_move_token(&self) -> Option<FriendMoveTokenRequest> {
        match self.direction {
            MoveTokenDirection::Incoming(_)=> None,
            MoveTokenDirection::Outgoing(ref outgoing_move_token) => {
                Some(outgoing_move_token.outgoing_move_token_request.clone())
            }
        }
    }
}