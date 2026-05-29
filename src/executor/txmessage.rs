/// Message types for inter-thread communication
/// Using repr(u8) for smaller memory footprint
#[repr(u8)]
pub enum TXMessageKind {
    #[allow(dead_code)]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn committed_carries_ids_and_duration_without_error() {
        let m = TXMessage::committed(7, 42, 1234);
        assert!(matches!(m.kind, TXMessageKind::COMMITTED));
        assert_eq!(m.tx_id, 7);
        assert_eq!(m.client_id, 42);
        assert_eq!(m.tx_duration_us, 1234);
        assert!(m.error.is_none());
    }

    #[test]
    fn error_populates_message_and_zeroes_duration() {
        let m = TXMessage::error(3, 9, "deadlock".to_string());
        assert!(matches!(m.kind, TXMessageKind::ERROR));
        assert_eq!(m.tx_id, 3);
        assert_eq!(m.client_id, 9);
        assert_eq!(m.tx_duration_us, 0);
        assert_eq!(m.error.as_deref(), Some("deadlock"));
    }

    #[test]
    fn terminate_is_zeroed_control_message() {
        let m = TXMessage::terminate_data_collector();
        assert!(matches!(m.kind, TXMessageKind::TERMINATE));
        assert_eq!(m.tx_id, 0);
        assert_eq!(m.client_id, 0);
        assert_eq!(m.tx_duration_us, 0);
        assert!(m.error.is_none());
    }

    #[test]
    fn end_of_rampup_is_zeroed_control_message() {
        let m = TXMessage::end_of_rampup();
        assert!(matches!(m.kind, TXMessageKind::ENDOFRAMPUP));
        assert_eq!(m.tx_id, 0);
        assert_eq!(m.client_id, 0);
        assert_eq!(m.tx_duration_us, 0);
        assert!(m.error.is_none());
    }
}
