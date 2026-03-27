use serde::Deserialize;

use crate::error::Result;

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

/// Load and validate the product catalog from products.toml.
pub fn load_catalog(_path: &str) -> Result<Vec<Product>> {
    todo!("Issue #2: implement catalog loading")
}
