use std::collections::HashSet;
use std::sync::Mutex;

use crate::catalog::Product;
use crate::error::{PurserError, Result};
use crate::messages::{Order, OrderType, PROTOCOL_VERSION};
use crate::providers::{PaymentMethod, ValidatedOrder, ValidatedOrderItem};

static SEEN_ORDERS: Mutex<Option<HashSet<(String, String)>>> = Mutex::new(None);

/// Validate an incoming order against the message schema and product catalog.
/// Returns a ValidatedOrder ready for payment creation, or an error describing
/// the validation failure.
pub fn validate_order(
    order: &Order,
    catalog: &[Product],
    customer_pubkey: &str,
) -> Result<ValidatedOrder> {
    // Version check
    if order.version != PROTOCOL_VERSION {
        return Err(PurserError::SchemaValidation(format!(
            "unsupported version: expected {}, got {}",
            PROTOCOL_VERSION, order.version
        )));
    }

    // Message type check
    if order.msg_type != "order" {
        return Err(PurserError::SchemaValidation(format!(
            "expected message type 'order', got '{}'",
            order.msg_type
        )));
    }

    // Order ID non-empty
    if order.order_id.is_empty() {
        return Err(PurserError::SchemaValidation(
            "order_id must not be empty".to_string(),
        ));
    }

    // Items non-empty
    if order.items.is_empty() {
        return Err(PurserError::SchemaValidation(
            "order must contain at least one item".to_string(),
        ));
    }

    // Physical orders require shipping
    if order.order_type == OrderType::Physical && order.shipping.is_none() {
        return Err(PurserError::SchemaValidation(
            "physical order requires shipping".to_string(),
        ));
    }

    // Service orders require service_details
    if order.order_type == OrderType::Service && order.service_details.is_none() {
        return Err(PurserError::SchemaValidation(
            "service order requires service_details".to_string(),
        ));
    }

    // Payment method must be non-empty and parseable
    if order.payment_method.is_empty() {
        return Err(PurserError::SchemaValidation(
            "payment_method must not be empty".to_string(),
        ));
    }
    let payment_method = PaymentMethod::from_str_loose(&order.payment_method).ok_or_else(|| {
        PurserError::SchemaValidation(format!(
            "unsupported payment method: '{}'",
            order.payment_method
        ))
    })?;

    // Validate each item against the catalog
    let mut validated_items = Vec::with_capacity(order.items.len());
    let mut total: f64 = 0.0;

    for item in &order.items {
        // Find product in catalog
        let product = catalog
            .iter()
            .find(|p| p.id == item.product_id)
            .ok_or_else(|| {
                PurserError::CatalogValidation(format!(
                    "product not found: '{}'",
                    item.product_id
                ))
            })?;

        // Product must be active
        if !product.active {
            return Err(PurserError::CatalogValidation(format!(
                "product is not active: '{}'",
                item.product_id
            )));
        }

        // Price check (skip for custom-quote products with price "0.00")
        if !product.requires_quote() && item.unit_price_usd != product.price_usd {
            return Err(PurserError::CatalogValidation("price mismatch".to_string()));
        }

        // Variant check
        if let Some(ref variant) = item.variant {
            if !product.variants.contains(variant) {
                return Err(PurserError::CatalogValidation(format!(
                    "invalid variant '{}' for product '{}'",
                    variant, item.product_id
                )));
            }
        }

        // Quantity must be > 0
        if item.quantity == 0 {
            return Err(PurserError::SchemaValidation(format!(
                "quantity must be greater than 0 for product '{}'",
                item.product_id
            )));
        }

        // Accumulate total
        let unit_price: f64 = item.unit_price_usd.parse().map_err(|_| {
            PurserError::SchemaValidation(format!(
                "invalid unit_price_usd '{}' for product '{}'",
                item.unit_price_usd, item.product_id
            ))
        })?;
        total += unit_price * item.quantity as f64;

        validated_items.push(ValidatedOrderItem {
            product_id: item.product_id.clone(),
            name: item.name.clone(),
            quantity: item.quantity,
            variant: item.variant.clone(),
            unit_price_usd: item.unit_price_usd.clone(),
        });
    }

    let total_usd = format!("{:.2}", total);

    Ok(ValidatedOrder {
        order_id: order.order_id.clone(),
        customer_pubkey: customer_pubkey.to_string(),
        items: validated_items,
        order_type: order.order_type.clone(),
        shipping: order.shipping.clone(),
        service_details: order.service_details.clone(),
        contact: order.contact.clone(),
        payment_method,
        total_usd,
        currency: order.currency.clone(),
        message: order.message.clone(),
    })
}

/// Check if an order_id has already been seen from this customer pubkey.
/// Returns true if already seen, false otherwise (and inserts the pair).
pub fn is_duplicate_order(order_id: &str, customer_pubkey: &str) -> bool {
    let mut guard = SEEN_ORDERS.lock().unwrap();
    let set = guard.get_or_insert_with(HashSet::new);
    let key = (order_id.to_string(), customer_pubkey.to_string());
    !set.insert(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{Product, ProductType};
    use crate::messages::{Order, OrderItem, OrderType, ServiceDetails, Shipping};

    fn make_physical_product() -> Product {
        Product {
            id: "heltec-v3".to_string(),
            name: "Heltec V3".to_string(),
            product_type: ProductType::Physical,
            price_usd: "59.99".to_string(),
            variants: vec!["single".to_string(), "3-pack".to_string()],
            active: true,
        }
    }

    fn make_service_product() -> Product {
        Product {
            id: "consulting".to_string(),
            name: "Consulting".to_string(),
            product_type: ProductType::Service,
            price_usd: "150.00".to_string(),
            variants: vec![],
            active: true,
        }
    }

    fn make_custom_quote_product() -> Product {
        Product {
            id: "custom-dev".to_string(),
            name: "Custom Development".to_string(),
            product_type: ProductType::Service,
            price_usd: "0.00".to_string(),
            variants: vec![],
            active: true,
        }
    }

    fn make_shipping() -> Shipping {
        Shipping {
            name: "Alice".to_string(),
            address_line_1: "123 Main St".to_string(),
            address_line_2: None,
            city: "Austin".to_string(),
            state: "TX".to_string(),
            zip: "78701".to_string(),
            country: "US".to_string(),
        }
    }

    fn make_service_details() -> ServiceDetails {
        ServiceDetails {
            description: "Need help with Rust".to_string(),
            preferred_date: None,
            notes: None,
        }
    }

    fn make_physical_order() -> Order {
        Order {
            version: 1,
            msg_type: "order".to_string(),
            order_id: "order-001".to_string(),
            items: vec![OrderItem {
                product_id: "heltec-v3".to_string(),
                name: "Heltec V3".to_string(),
                quantity: 2,
                variant: Some("single".to_string()),
                unit_price_usd: "59.99".to_string(),
            }],
            order_type: OrderType::Physical,
            shipping: Some(make_shipping()),
            service_details: None,
            contact: Some("alice@example.com".to_string()),
            payment_method: "fiat".to_string(),
            currency: "USD".to_string(),
            message: None,
        }
    }

    fn make_service_order() -> Order {
        Order {
            version: 1,
            msg_type: "order".to_string(),
            order_id: "order-002".to_string(),
            items: vec![OrderItem {
                product_id: "consulting".to_string(),
                name: "Consulting".to_string(),
                quantity: 1,
                variant: None,
                unit_price_usd: "150.00".to_string(),
            }],
            order_type: OrderType::Service,
            shipping: None,
            service_details: Some(make_service_details()),
            contact: None,
            payment_method: "lightning".to_string(),
            currency: "USD".to_string(),
            message: None,
        }
    }

    #[test]
    fn test_valid_physical_order() {
        let catalog = vec![make_physical_product()];
        let order = make_physical_order();
        let result = validate_order(&order, &catalog, "npub1alice");
        assert!(result.is_ok());
        let validated = result.unwrap();
        assert_eq!(validated.order_id, "order-001");
        assert_eq!(validated.customer_pubkey, "npub1alice");
        assert_eq!(validated.total_usd, "119.98");
    }

    #[test]
    fn test_valid_service_order() {
        let catalog = vec![make_service_product()];
        let order = make_service_order();
        let result = validate_order(&order, &catalog, "npub1bob");
        assert!(result.is_ok());
        let validated = result.unwrap();
        assert_eq!(validated.order_id, "order-002");
        assert_eq!(validated.total_usd, "150.00");
    }

    #[test]
    fn test_missing_order_id() {
        let catalog = vec![make_physical_product()];
        let mut order = make_physical_order();
        order.order_id = "".to_string();
        let result = validate_order(&order, &catalog, "npub1alice");
        assert!(matches!(result, Err(PurserError::SchemaValidation(_))));
    }

    #[test]
    fn test_physical_without_shipping() {
        let catalog = vec![make_physical_product()];
        let mut order = make_physical_order();
        order.shipping = None;
        let result = validate_order(&order, &catalog, "npub1alice");
        assert!(matches!(result, Err(PurserError::SchemaValidation(ref msg)) if msg.contains("shipping")));
    }

    #[test]
    fn test_service_without_service_details() {
        let catalog = vec![make_service_product()];
        let mut order = make_service_order();
        order.service_details = None;
        let result = validate_order(&order, &catalog, "npub1bob");
        assert!(matches!(result, Err(PurserError::SchemaValidation(ref msg)) if msg.contains("service_details")));
    }

    #[test]
    fn test_unknown_product_id() {
        let catalog = vec![make_physical_product()];
        let mut order = make_physical_order();
        order.items[0].product_id = "nonexistent".to_string();
        let result = validate_order(&order, &catalog, "npub1alice");
        assert!(matches!(result, Err(PurserError::CatalogValidation(_))));
    }

    #[test]
    fn test_price_mismatch() {
        let catalog = vec![make_physical_product()];
        let mut order = make_physical_order();
        order.items[0].unit_price_usd = "99.99".to_string();
        let result = validate_order(&order, &catalog, "npub1alice");
        assert!(matches!(result, Err(PurserError::CatalogValidation(ref msg)) if msg.contains("price mismatch")));
    }

    #[test]
    fn test_invalid_variant() {
        let catalog = vec![make_physical_product()];
        let mut order = make_physical_order();
        order.items[0].variant = Some("nonexistent-variant".to_string());
        let result = validate_order(&order, &catalog, "npub1alice");
        assert!(matches!(result, Err(PurserError::CatalogValidation(ref msg)) if msg.contains("invalid variant")));
    }

    #[test]
    fn test_custom_quote_product() {
        let catalog = vec![make_custom_quote_product()];
        let order = Order {
            version: 1,
            msg_type: "order".to_string(),
            order_id: "order-003".to_string(),
            items: vec![OrderItem {
                product_id: "custom-dev".to_string(),
                name: "Custom Development".to_string(),
                quantity: 1,
                variant: None,
                unit_price_usd: "5000.00".to_string(),
            }],
            order_type: OrderType::Service,
            shipping: None,
            service_details: Some(make_service_details()),
            contact: None,
            payment_method: "lightning".to_string(),
            currency: "USD".to_string(),
            message: None,
        };
        let result = validate_order(&order, &catalog, "npub1charlie");
        assert!(result.is_ok());
        let validated = result.unwrap();
        assert_eq!(validated.total_usd, "5000.00");
    }

    #[test]
    fn test_wrong_version() {
        let catalog = vec![make_physical_product()];
        let mut order = make_physical_order();
        order.version = 99;
        let result = validate_order(&order, &catalog, "npub1alice");
        assert!(matches!(result, Err(PurserError::SchemaValidation(_))));
    }

    #[test]
    fn test_unsupported_payment_method() {
        let catalog = vec![make_physical_product()];
        let mut order = make_physical_order();
        order.payment_method = "dogecoin".to_string();
        let result = validate_order(&order, &catalog, "npub1alice");
        assert!(matches!(result, Err(PurserError::SchemaValidation(_))));
    }

    #[test]
    fn test_zero_quantity() {
        let catalog = vec![make_physical_product()];
        let mut order = make_physical_order();
        order.items[0].quantity = 0;
        let result = validate_order(&order, &catalog, "npub1alice");
        assert!(matches!(
            result,
            Err(PurserError::SchemaValidation(_))
        ));
    }

    #[test]
    fn test_duplicate_order_detection() {
        // Use unique IDs to avoid interference from other tests
        let order_id = "dup-test-unique-id-12345";
        let pubkey = "npub1duptest";
        assert!(!is_duplicate_order(order_id, pubkey));
        assert!(is_duplicate_order(order_id, pubkey));
        // Different pubkey should not be duplicate
        assert!(!is_duplicate_order(order_id, "npub1other"));
    }

    #[test]
    fn test_total_calculation() {
        let catalog = vec![make_physical_product(), make_service_product()];
        let order = Order {
            version: 1,
            msg_type: "order".to_string(),
            order_id: "order-total-test".to_string(),
            items: vec![
                OrderItem {
                    product_id: "heltec-v3".to_string(),
                    name: "Heltec V3".to_string(),
                    quantity: 3,
                    variant: Some("single".to_string()),
                    unit_price_usd: "59.99".to_string(),
                },
                OrderItem {
                    product_id: "consulting".to_string(),
                    name: "Consulting".to_string(),
                    quantity: 2,
                    variant: None,
                    unit_price_usd: "150.00".to_string(),
                },
            ],
            order_type: OrderType::Physical,
            shipping: Some(make_shipping()),
            service_details: None,
            contact: None,
            payment_method: "fiat".to_string(),
            currency: "USD".to_string(),
            message: None,
        };
        let result = validate_order(&order, &catalog, "npub1calc");
        assert!(result.is_ok());
        let validated = result.unwrap();
        // 3 * 59.99 + 2 * 150.00 = 179.97 + 300.00 = 479.97
        assert_eq!(validated.total_usd, "479.97");
    }
}
