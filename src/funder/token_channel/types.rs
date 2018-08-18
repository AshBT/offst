use std::cmp;

use im::hashmap::HashMap as ImHashMap;

// use num_bigint::BigUint;

use crypto::identity::PublicKey;
use crypto::uid::Uid;
use crypto::rand_values::{RandValue};

use proto::funder::ChannelToken;

use utils::safe_arithmetic::SafeArithmetic;

use super::super::types::{PendingFriendRequest, FriendTcOp};
use super::super::messages::RequestsStatus;

/// The maximum possible funder debt.
/// We don't use the full u128 because i128 can not go beyond this value.
pub const MAX_FUNDER_DEBT: u128 = (1 << 127) - 1;

#[derive(Clone)]
pub struct FriendMoveTokenInner {
    pub operations: Vec<FriendTcOp>,
    pub old_token: ChannelToken,
    pub rand_nonce: RandValue,
}

#[derive(Clone)]
pub struct TcIdents {
    /// My public key
    pub local_public_key: PublicKey,
    /// Friend's public key
    pub remote_public_key: PublicKey,
}

#[derive(Clone)]
pub struct TcBalance {
    /// Amount of credits this side has against the remote side.
    /// The other side keeps the negation of this value.
    pub balance: i128,
    /// Maximum possible remote debt
    pub remote_max_debt: u128,
    /// Maximum possible local debt
    pub local_max_debt: u128,
    /// Frozen credits by our side
    pub local_pending_debt: u128,
    /// Frozen credits by the remote side
    pub remote_pending_debt: u128,
}

impl TcBalance {
    fn new(balance: i128) -> TcBalance {
        TcBalance {
            balance,
            remote_max_debt: cmp::max(balance, 0) as u128,
            local_max_debt: cmp::min(-balance, 0) as u128,
            local_pending_debt: 0,
            remote_pending_debt: 0,
        }
    }
}

#[derive(Clone)]
pub struct TcPendingRequests {
    /// Pending requests that were opened locally and not yet completed
    pub pending_local_requests: ImHashMap<Uid, PendingFriendRequest>,
    /// Pending requests that were opened remotely and not yet completed
    pub pending_remote_requests: ImHashMap<Uid, PendingFriendRequest>,
}

impl TcPendingRequests {
    fn new() -> TcPendingRequests {
        TcPendingRequests {
            pending_local_requests: ImHashMap::new(),
            pending_remote_requests: ImHashMap::new(),
        }
    }
}

#[derive(Clone)]
pub struct TcRequestsStatus {
    // Local is open/closed for incoming requests:
    pub local: RequestsStatus,
    // Remote is open/closed for incoming requests:
    pub remote: RequestsStatus,
}

impl TcRequestsStatus {
    fn new() -> TcRequestsStatus {
        TcRequestsStatus {
            local: RequestsStatus::Closed,
            remote: RequestsStatus::Closed,
        }
    }
}


#[derive(Clone)]
pub struct TokenChannelState {
    pub idents: TcIdents,
    pub balance: TcBalance,
    pub pending_requests: TcPendingRequests,
    pub requests_status: TcRequestsStatus,
}

#[derive(Clone)]
pub struct TokenChannel {
    state: TokenChannelState,
}

pub enum TcMutation {
    SetLocalRequestsStatus(RequestsStatus),
    SetRemoteRequestsStatus(RequestsStatus),
    SetLocalMaxDebt(u128),
    SetRemoteMaxDebt(u128),
    SetBalance(i128),
    InsertLocalPendingRequest(PendingFriendRequest),
    RemoveLocalPendingRequest(Uid),
    InsertRemotePendingRequest(PendingFriendRequest),
    RemoveRemotePendingRequest(Uid),
    SetLocalPendingDebt(u128),
    SetRemotePendingDebt(u128),
}


impl TokenChannel {
    pub fn new(local_public_key: &PublicKey, 
           remote_public_key: &PublicKey, 
           balance: i128) -> TokenChannel {

        TokenChannel {
            state: TokenChannelState {
                idents: TcIdents {
                    local_public_key: local_public_key.clone(),
                    remote_public_key: remote_public_key.clone(),
                },
                balance: TcBalance::new(balance),
                pending_requests: TcPendingRequests::new(),
                requests_status: TcRequestsStatus::new(),
            }
        }
    }

    /// Calculate required balance for reset.
    /// This would be current balance plus additional future profits.
    pub fn balance_for_reset(&self) -> i128 {
        self.state.balance.balance
            .checked_add_unsigned(self.state.balance.remote_pending_debt)
            .expect("Overflow when calculating balance_for_reset")
    }

    pub fn state(&self) -> &TokenChannelState {
        &self.state
    }

    pub fn mutate(&mut self, tc_mutation: &TcMutation) {
        match tc_mutation {
            TcMutation::SetLocalRequestsStatus(requests_status) => 
                self.set_local_requests_status(requests_status.clone()),
            TcMutation::SetRemoteRequestsStatus(requests_status) => 
                self.set_remote_requests_status(requests_status.clone()),
            TcMutation::SetLocalMaxDebt(proposed_max_debt) => 
                self.set_local_max_debt(*proposed_max_debt),
            TcMutation::SetRemoteMaxDebt(proposed_max_debt) => 
                self.set_remote_max_debt(*proposed_max_debt),
            TcMutation::SetBalance(balance) => 
                self.set_balance(*balance),
            TcMutation::InsertLocalPendingRequest(pending_friend_request) =>
                self.insert_local_pending_request(pending_friend_request),
            TcMutation::RemoveLocalPendingRequest(request_id) =>
                self.remove_local_pending_request(request_id),
            TcMutation::InsertRemotePendingRequest(pending_friend_request) =>
                self.insert_remote_pending_request(pending_friend_request),
            TcMutation::RemoveRemotePendingRequest(request_id) =>
                self.remove_remote_pending_request(request_id),
            TcMutation::SetLocalPendingDebt(local_pending_debt) =>
                self.set_local_pending_debt(*local_pending_debt),
            TcMutation::SetRemotePendingDebt(remote_pending_debt) =>
                self.set_remote_pending_debt(*remote_pending_debt),
        }
    }

    fn set_local_requests_status(&mut self, requests_status: RequestsStatus) {
        self.state.requests_status.local = requests_status;
    }

    fn set_remote_requests_status(&mut self, requests_status: RequestsStatus) {
        self.state.requests_status.remote = requests_status;
    }

    fn set_remote_max_debt(&mut self, proposed_max_debt: u128) { 
        self.state.balance.remote_max_debt = proposed_max_debt;
    }

    fn set_local_max_debt(&mut self, proposed_max_debt: u128) {
        self.state.balance.local_max_debt = proposed_max_debt;
    }

    fn set_balance(&mut self, balance: i128) {
        self.state.balance.balance = balance;
    }

    fn insert_remote_pending_request(&mut self, pending_friend_request: &PendingFriendRequest) {
        self.state.pending_requests.pending_remote_requests.insert(
            pending_friend_request.request_id,
            pending_friend_request.clone());
    }

    fn remove_remote_pending_request(&mut self, request_id: &Uid) {
        let _ = self.state.pending_requests.pending_remote_requests.remove(
            request_id);
    }

    fn insert_local_pending_request(&mut self, pending_friend_request: &PendingFriendRequest) {
        self.state.pending_requests.pending_local_requests.insert(
            pending_friend_request.request_id,
            pending_friend_request.clone());
    }

    fn remove_local_pending_request(&mut self, request_id: &Uid) {
        let _ = self.state.pending_requests.pending_local_requests.remove(
            request_id);
    }

    fn set_remote_pending_debt(&mut self, remote_pending_debt: u128) {
        self.state.balance.remote_pending_debt = remote_pending_debt;
    }


    fn set_local_pending_debt(&mut self, local_pending_debt: u128) {
        self.state.balance.local_pending_debt = local_pending_debt;
    }
}