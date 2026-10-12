use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, str::FromStr};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexItem {
    pub product_id: String,
    pub category: String,
    pub quantity: String,
    pub base_unit_price: String,
    pub current_unit_price: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemContribution {
    pub product_id: String,
    pub category: String,
    pub base_cost: String,
    pub current_cost: Option<String>,
    pub price_change_percent: Option<String>,
    pub absolute_contribution: Option<String>,
    pub index_points: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CategoryContribution {
    pub category: String,
    pub base_cost: String,
    pub current_cost: String,
    pub index: String,
    pub index_points: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexResult {
    pub status: String,
    pub base_cost: String,
    pub current_cost: Option<String>,
    pub index: Option<String>,
    pub change_from_base_percent: Option<String>,
    pub coverage_count_percent: String,
    pub coverage_base_value_percent: String,
    pub comparable_subset_index: Option<String>,
    pub items: Vec<ItemContribution>,
    pub categories: Vec<CategoryContribution>,
}

pub fn calculate(items: &[IndexItem]) -> Result<IndexResult, String> {
    if items.is_empty() {
        return Err("индекс не может быть рассчитан без товаров".to_owned());
    }
    let mut sorted = items.to_vec();
    sorted.sort_by(|left, right| left.product_id.cmp(&right.product_id));
    let mut base_total = Decimal::ZERO;
    let mut current_total = Decimal::ZERO;
    let mut comparable_base = Decimal::ZERO;
    let mut comparable_current = Decimal::ZERO;
    let mut available = 0usize;
    let mut contributions = Vec::new();
    let mut category_values = BTreeMap::<String, (Decimal, Decimal)>::new();

    for item in &sorted {
        let quantity = decimal(&item.quantity)?;
        let base_price = decimal(&item.base_unit_price)?;
        let base_cost = quantity * base_price;
        base_total += base_cost;
        let (current_cost, price_change, absolute) = if let Some(raw) = &item.current_unit_price {
            let current_price = decimal(raw)?;
            let cost = quantity * current_price;
            current_total += cost;
            comparable_base += base_cost;
            comparable_current += cost;
            available += 1;
            let entry = category_values.entry(item.category.clone()).or_default();
            entry.0 += base_cost;
            entry.1 += cost;
            (
                Some(cost),
                Some((current_price / base_price - Decimal::ONE) * Decimal::from(100)),
                Some(cost - base_cost),
            )
        } else {
            (None, None, None)
        };
        contributions.push(ItemContribution {
            product_id: item.product_id.clone(),
            category: item.category.clone(),
            base_cost: show(base_cost),
            current_cost: current_cost.map(show),
            price_change_percent: price_change.map(show),
            absolute_contribution: absolute.map(show),
            index_points: absolute
                .map(|value| show(Decimal::from(100) * value / base_total.max(base_cost))),
        });
    }
    // Пересчитываем пункты после получения полного знаменателя.
    for contribution in &mut contributions {
        contribution.index_points = contribution
            .absolute_contribution
            .as_deref()
            .map(decimal)
            .transpose()?
            .map(|value| show(Decimal::from(100) * value / base_total));
    }
    let complete = available == sorted.len();
    let index = complete.then(|| Decimal::from(100) * current_total / base_total);
    let categories = if complete {
        category_values
            .into_iter()
            .map(|(category, (base, current))| CategoryContribution {
                category,
                base_cost: show(base),
                current_cost: show(current),
                index: show(Decimal::from(100) * current / base),
                index_points: show(Decimal::from(100) * (current - base) / base_total),
            })
            .collect()
    } else {
        Vec::new()
    };
    Ok(IndexResult {
        status: if complete { "Complete" } else { "Incomplete" }.to_owned(),
        base_cost: show(base_total),
        current_cost: complete.then(|| show(current_total)),
        index: index.map(show),
        change_from_base_percent: index.map(|value| show(value - Decimal::from(100))),
        coverage_count_percent: show(
            Decimal::from(100) * Decimal::from(available) / Decimal::from(sorted.len()),
        ),
        coverage_base_value_percent: show(Decimal::from(100) * comparable_base / base_total),
        comparable_subset_index: (available > 0)
            .then(|| show(Decimal::from(100) * comparable_current / comparable_base)),
        items: contributions,
        categories,
    })
}

fn decimal(value: &str) -> Result<Decimal, String> {
    Decimal::from_str(value).map_err(|error| error.to_string())
}

fn show(value: Decimal) -> String {
    value.round_dp(6).normalize().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(factor: Option<&str>) -> Vec<IndexItem> {
        [("a", "Молоко", "1", "100"), ("b", "Крупы", "2", "50")]
            .into_iter()
            .map(|(id, category, quantity, base)| IndexItem {
                product_id: id.to_owned(),
                category: category.to_owned(),
                quantity: quantity.to_owned(),
                base_unit_price: base.to_owned(),
                current_unit_price: factor
                    .map(|factor| show(decimal(base).unwrap() * decimal(factor).unwrap())),
            })
            .collect()
    }

    #[test]
    fn base_unchanged_and_plus_minus_ten_percent() {
        assert_eq!(
            calculate(&items(Some("1"))).unwrap().index.as_deref(),
            Some("100")
        );
        assert_eq!(
            calculate(&items(Some("1.1"))).unwrap().index.as_deref(),
            Some("110")
        );
        assert_eq!(
            calculate(&items(Some("0.9"))).unwrap().index.as_deref(),
            Some("90")
        );
    }

    #[test]
    fn incomplete_period_has_only_explicit_subset_index() {
        let mut values = items(Some("1.1"));
        values[1].current_unit_price = None;
        let result = calculate(&values).unwrap();
        assert_eq!(result.status, "Incomplete");
        assert!(result.index.is_none());
        assert_eq!(result.comparable_subset_index.as_deref(), Some("110"));
    }

    #[test]
    fn contributions_and_order_are_deterministic() {
        let original = items(Some("1.1"));
        let mut reversed = original.clone();
        reversed.reverse();
        let left = calculate(&original).unwrap();
        let right = calculate(&reversed).unwrap();
        assert_eq!(left.index, right.index);
        assert_eq!(left.items[0].product_id, "a");
        let sum = left
            .items
            .iter()
            .map(|item| decimal(item.absolute_contribution.as_deref().unwrap()).unwrap())
            .sum::<Decimal>();
        assert_eq!(sum, Decimal::from(20));
    }

    #[test]
    fn package_shrink_changes_unit_price_even_if_package_price_is_same() {
        let base_unit = Decimal::from(100);
        let current_unit = Decimal::from(100) / Decimal::new(9, 1);
        let item = IndexItem {
            product_id: "x".to_owned(),
            category: "Тест".to_owned(),
            quantity: "1".to_owned(),
            base_unit_price: base_unit.to_string(),
            current_unit_price: Some(current_unit.to_string()),
        };
        assert!(
            decimal(calculate(&[item]).unwrap().index.as_deref().unwrap()).unwrap()
                > Decimal::from(111)
        );
    }
}
