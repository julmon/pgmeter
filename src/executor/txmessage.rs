/// Message types for inter-thread communication
/// Using repr(u8) for smaller memory footprint
#[repr(u8)]
pub enum TXMessageKind {
    DEFAULT = 0,
    COMMITTED = 1,
    ERROR = 2,
    TERMINATE = 3,
    ENDOFRAMPUP = 4,
}

/// Transaction message for communication between benchmark clients and data collector.
/// Optimized for minimal allocations in the hot path.
pub struct TXMessage {
    pub kind: TXMessageKind,
    pub tx_id: u16,
    pub client_id: u32,
    pub tx_duration_us: u128,
    /// Error message - only populated for ERROR kind (avoids allocation for commits)
    pub error: Option<String>,
}

impl TXMessage {
    /// Create a message for a committed transaction (hot path - no allocations)
    #[inline]
    pub fn committed(tx_id: u16, client_id: u32, tx_duration_us: u128) -> TXMessage {
        TXMessage {
            kind: TXMessageKind::COMMITTED,
            tx_id,
            client_id,
            tx_duration_us,
            error: None,
        }
    }

    /// Create a message for a failed transaction
    #[inline]
    pub fn error(tx_id: u16, client_id: u32, error: String) -> TXMessage {
        TXMessage {
            kind: TXMessageKind::ERROR,
            tx_id,
            client_id,
            tx_duration_us: 0,
            error: Some(error),
        }
    }

    /// Signal to terminate the data collector
    #[inline]
    pub fn terminate_data_collector() -> TXMessage {
        TXMessage {
            kind: TXMessageKind::TERMINATE,
            tx_id: 0,
            client_id: 0,
            tx_duration_us: 0,
            error: None,
        }
    }

    /// Signal end of ramp-up period
    #[inline]
    pub fn end_of_rampup() -> TXMessage {
        TXMessage {
            kind: TXMessageKind::ENDOFRAMPUP,
            tx_id: 0,
            client_id: 0,
            tx_duration_us: 0,
            error: None,
        }
    }
}
