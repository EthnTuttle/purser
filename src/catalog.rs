use serde::Deserialize;

use crate::error::{PurserError, Result};

/// A product from the static product catalog (products.toml).
#[derive(Debug, Clone, Deserialize)]
pub struct Product {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub product_type: ProductType,
    pub price_usd: String,
    #[serde(default)]
    pub variants: Vec<String>,
    #[serde(default = "default_active")]
    pub active: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProductType {
    Physical,
    Service,
}

fn default_active() -> bool { true }

impl Product {
    /// Returns true if this product requires a custom quote (price_usd = "0.00").
    pub fn requires_quote(&self) -> bool {
        self.price_usd == "0.00"
    }
}

/// Wrapper struct for deserializing the `[[products]]` array from TOML.
#[derive(Deserialize)]
struct Catalog {
    products: Vec<Product>,
}

/// Returns true if `s` is a valid decimal string (digits with at most one dot).
fn is_valid_decimal(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let mut dot_count = 0u32;
    for c in s.chars() {
        if c == '.' {
            dot_count += 1;
            if dot_count > 1 {
                return false;
            }
        } else if !c.is_ascii_digit() {
            return false;
        }
    }
    true
}

/// Load and validate the product catalog from products.toml.
///
/// Validations:
/// - Every product must have a non-empty `id` and `name`
/// - No duplicate product `id` values
/// - `price_usd` must be a valid decimal string (digits and at most one dot)
pub fn load_catalog(path: &str) -> Result<Vec<Product>> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| PurserError::Catalog(format!("failed to read catalog file '{}': {}", path, e)))?;

    let catalog: Catalog = toml::from_str(&content)
        .map_err(|e| PurserError::Catalog(format!("failed to parse catalog TOML: {}", e)))?;

    let products = catalog.products;

    // Validate each product
    let mut seen_ids = std::collections::HashSet::new();
    for product in &products {
        if product.id.is_empty() {
            return Err(PurserError::Catalog("product id must not be empty".to_string()));
        }
        if product.name.is_empty() {
            return Err(PurserError::Catalog(
                format!("product '{}' has an empty name", product.id),
            ));
        }
        if !seen_ids.insert(&product.id) {
            return Err(PurserError::Catalog(
                format!("duplicate product id: '{}'", product.id),
            ));
        }
        if !is_valid_decimal(&product.price_usd) {
            return Err(PurserError::Catalog(
                format!("product '{}' has invalid price_usd: '{}'", product.id, product.price_usd),
            ));
        }
    }

    tracing::info!(product_count = products.len(), "loaded product catalog");

    Ok(products)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp_catalog(name: &str, content: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("purser_cat_{}_{}", name, std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("products.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        path
    }

    #[test]
    fn test_load_valid_catalog() {
        let path = write_temp_catalog("valid", r#"
[[products]]
id = "widget-a"
name = "Widget A"
type = "physical"
price_usd = "19.99"
variants = ["small", "large"]
active = true

[[products]]
id = "service-b"
name = "Service B"
type = "service"
price_usd = "0.00"
variants = []
active = true
"#);
        let products = load_catalog(path.to_str().unwrap()).unwrap();
        assert_eq!(products.len(), 2);
        assert_eq!(products[0].id, "widget-a");
        assert_eq!(products[1].price_usd, "0.00");
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn test_load_catalog_malformed() {
        let path = write_temp_catalog("malformed", r#"
[[products]]
id = "no-name"
type = "physical"
price_usd = "10.00"
"#);
        let result = load_catalog(path.to_str().unwrap());
        assert!(result.is_err(), "expected error for missing name field");
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn test_load_catalog_duplicate_ids() {
        let path = write_temp_catalog("dup", r#"
[[products]]
id = "dup-item"
name = "Item 1"
type = "physical"
price_usd = "5.00"

[[products]]
id = "dup-item"
name = "Item 2"
type = "physical"
price_usd = "10.00"
"#);
        let result = load_catalog(path.to_str().unwrap());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("duplicate"), "expected duplicate error, got: {}", err);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn test_load_catalog_invalid_price() {
        let path = write_temp_catalog("badprice", r#"
[[products]]
id = "bad-price"
name = "Bad Price Item"
type = "physical"
price_usd = "abc"
"#);
        let result = load_catalog(path.to_str().unwrap());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("invalid price_usd"), "expected price error, got: {}", err);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn test_product_requires_quote() {
        let product = Product {
            id: "quote-item".to_string(),
            name: "Quote Item".to_string(),
            product_type: ProductType::Service,
            price_usd: "0.00".to_string(),
            variants: vec![],
            active: true,
        };
        assert!(product.requires_quote());

        let priced = Product {
            id: "priced-item".to_string(),
            name: "Priced Item".to_string(),
            product_type: ProductType::Physical,
            price_usd: "49.99".to_string(),
            variants: vec![],
            active: true,
        };
        assert!(!priced.requires_quote());
    }
}
