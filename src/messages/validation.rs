use crate::catalog::Product;
use crate::error::Result;
use crate::messages::Order;
use crate::providers::ValidatedOrder;

/// Validate an incoming order against the message schema and product catalog.
/// Returns a ValidatedOrder ready for payment creation, or an error describing
/// the validation failure.
pub fn validate_order(
    _order: &Order,
    _catalog: &[Product],
    _customer_pubkey: &str,
) -> Result<ValidatedOrder> {
    todo!("Issue #3: implement order validation")
}

/// Check if an order_id has already been seen from this customer pubkey.
pub fn is_duplicate_order(_order_id: &str, _customer_pubkey: &str) -> bool {
    todo!("Issue #3: implement duplicate detection")
}
