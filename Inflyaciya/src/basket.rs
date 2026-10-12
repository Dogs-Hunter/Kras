use chrono::Utc;
use rust_decimal::Decimal;
use std::{collections::BTreeMap, fs, path::Path, str::FromStr};

use crate::domain::{BasketConfig, BasketItem, Budget, Product, ProductCatalog};

#[derive(Clone)]
struct State {
    selected: Vec<usize>,
}

pub fn optimize(catalog: &ProductCatalog, budget: &Budget) -> Result<BasketConfig, String> {
    optimize_with_strategy(catalog, budget, &std::collections::BTreeSet::new(), false)
}

#[allow(dead_code)]
pub fn optimize_balanced(
    catalog: &ProductCatalog,
    budget: &Budget,
    excluded: &std::collections::BTreeSet<String>,
) -> Result<BasketConfig, String> {
    optimize_with_strategy(catalog, budget, excluded, true)
}

fn optimize_with_strategy(
    catalog: &ProductCatalog,
    budget: &Budget,
    excluded: &std::collections::BTreeSet<String>,
    balanced: bool,
) -> Result<BasketConfig, String> {
    let mut by_group = BTreeMap::<String, Vec<&Product>>::new();
    for product in &catalog.products {
        if matches!(product.normalized_unit.as_str(), "kg" | "l")
            && !excluded.contains(&product.source_product_id)
        {
            by_group
                .entry(product.group.clone())
                .or_default()
                .push(product);
        }
    }
    for products in by_group.values_mut() {
        products.sort_by(|left, right| left.source_product_id.cmp(&right.source_product_id));
    }
    let mut candidates = Vec::<&Product>::new();
    let grouped_indices = by_group
        .into_values()
        .map(|products| {
            products
                .into_iter()
                .map(|product| {
                    let index = candidates.len();
                    candidates.push(product);
                    index
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();

    let maximum_cents = to_cents(budget.maximum)?;
    let minimum_cents = to_cents(budget.minimum)?;
    let target_cents = to_cents(budget.target)?;
    let mut states = BTreeMap::<i64, State>::new();
    states.insert(
        0,
        State {
            selected: Vec::new(),
        },
    );
    for group in grouped_indices {
        let snapshot = states.clone();
        for (cost, state) in snapshot {
            for index in &group {
                let product_cost = to_cents(candidates[*index].package_price_decimal()?)?;
                let new_cost = cost + product_cost;
                if new_cost > maximum_cents {
                    continue;
                }
                let mut selected = state.selected.clone();
                selected.push(*index);
                match states.get(&new_cost) {
                    Some(existing)
                        if better_or_equal(
                            existing,
                            &selected,
                            &candidates,
                            balanced,
                            new_cost,
                        ) => {}
                    _ => {
                        states.insert(new_cost, State { selected });
                    }
                }
            }
        }
    }

    let best = states
        .iter()
        .filter(|(cost, _)| **cost >= minimum_cents && **cost <= maximum_cents)
        .max_by(|(left_cost, left), (right_cost, right)| {
            left.selected
                .len()
                .cmp(&right.selected.len())
                .then_with(|| {
                    if balanced {
                        let left_score = concentration_score(left, &candidates, **left_cost);
                        let right_score = concentration_score(right, &candidates, **right_cost);
                        right_score.cmp(&left_score)
                    } else {
                        std::cmp::Ordering::Equal
                    }
                })
                .then_with(|| {
                    (target_cents - **right_cost)
                        .abs()
                        .cmp(&(target_cents - **left_cost).abs())
                })
                .then_with(|| right_cost.cmp(left_cost))
        });
    let Some((cost, state)) = best else {
        let reachable = states
            .iter()
            .max_by_key(|(_, state)| state.selected.len())
            .map(|(cost, state)| (cost, state.selected.len()))
            .unwrap_or((&0, 0));
        return Err(format!(
            "невозможно собрать корзину в диапазоне {}–{} ₽; лучший достижимый вариант содержит {} групп и стоит {} ₽",
            budget.minimum,
            budget.maximum,
            reachable.1,
            from_cents(*reachable.0)
        ));
    };
    let mut items = state
        .selected
        .iter()
        .map(|index| {
            let product = candidates[*index];
            BasketItem {
                group: product.group.clone(),
                source_product_id: product.source_product_id.clone(),
                mcp_product_id: product.mcp_product_id,
                name: product.name.clone(),
                url: product.url.clone(),
                quantity_packages: 1,
                package_price: product.package_price.clone(),
                package_amount: product.package_amount.clone(),
                package_unit: product.package_unit.clone(),
                package_fingerprint: product.package_fingerprint(),
            }
        })
        .collect::<Vec<_>>();
    items.sort_by(|left, right| left.group.cmp(&right.group));
    let created_at = Utc::now();
    Ok(BasketConfig {
        schema_version: 1,
        basket_id: format!("basket-{}", created_at.format("%Y%m%dT%H%M%SZ")),
        status: "draft".to_owned(),
        created_at: created_at.to_rfc3339(),
        approved_at: String::new(),
        minimum_wage_rub: budget.minimum_wage.to_string(),
        target_rub: budget.target.to_string(),
        minimum_rub: budget.minimum.to_string(),
        maximum_rub: budget.maximum.to_string(),
        total_rub: from_cents(*cost).to_string(),
        items,
    })
}

fn concentration_score(state: &State, products: &[&Product], total_cents: i64) -> i128 {
    let squares = state
        .selected
        .iter()
        .map(|index| {
            let cents = to_cents(products[*index].package_price_decimal().unwrap_or_default())
                .unwrap_or_default() as i128;
            cents * cents
        })
        .sum::<i128>();
    if total_cents == 0 {
        i128::MAX
    } else {
        squares * 1_000_000_000 / (i128::from(total_cents) * i128::from(total_cents))
    }
}

fn better_or_equal(
    existing: &State,
    candidate: &[usize],
    products: &[&Product],
    balanced: bool,
    cost: i64,
) -> bool {
    existing.selected.len() > candidate.len()
        || (existing.selected.len() == candidate.len()
            && ((!balanced
                && product_key(&existing.selected, products) <= product_key(candidate, products))
                || (balanced
                    && (concentration_score(existing, products, cost)
                        < concentration_score(
                            &State {
                                selected: candidate.to_vec(),
                            },
                            products,
                            cost,
                        )
                        || (concentration_score(existing, products, cost)
                            == concentration_score(
                                &State {
                                    selected: candidate.to_vec(),
                                },
                                products,
                                cost,
                            )
                            && product_key(&existing.selected, products)
                                <= product_key(candidate, products))))))
}

fn product_key(indices: &[usize], products: &[&Product]) -> String {
    indices
        .iter()
        .map(|index| products[*index].source_product_id.as_str())
        .collect::<Vec<_>>()
        .join("|")
}

fn to_cents(value: Decimal) -> Result<i64, String> {
    let cents = (value * Decimal::from(100u16)).round();
    i64::from_str(&cents.to_string()).map_err(|error| format!("invalid cents: {error}"))
}

fn from_cents(value: i64) -> Decimal {
    Decimal::from(value) / Decimal::from(100u16)
}

pub fn load(path: &str) -> Result<BasketConfig, String> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("не удалось прочитать {path}: {error}"))?;
    toml::from_str(&text).map_err(|error| format!("ошибка TOML корзины: {error}"))
}

pub fn save(path: &str, basket: &BasketConfig) -> Result<(), String> {
    if let Some(parent) = Path::new(path).parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let text = toml::to_string_pretty(basket).map_err(|error| error.to_string())?;
    fs::write(path, text).map_err(|error| format!("не удалось записать {path}: {error}"))
}

pub fn approve(path: &str) -> Result<BasketConfig, String> {
    let mut basket = load(path)?;
    match basket.status.as_str() {
        "approved" => return Ok(basket),
        "draft" => {}
        status => return Err(format!("корзина имеет неподдерживаемый статус {status:?}")),
    }
    basket.status = "approved".to_owned();
    basket.approved_at = Utc::now().to_rfc3339();
    save(path, &basket)?;
    Ok(basket)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn product(group: &str, id: u64, price: &str) -> Product {
        Product {
            source_product_id: format!("vkusvill:{id}"),
            mcp_product_id: id,
            xml_id: Some(id),
            name: format!("Товар {id}"),
            brand: None,
            category: "Еда".to_owned(),
            group: group.to_owned(),
            query: group.to_owned(),
            url: "https://example.invalid".to_owned(),
            package_price: price.to_owned(),
            old_price: None,
            discount_percent: None,
            discount_info: None,
            package_amount: "1".to_owned(),
            package_unit: "кг".to_owned(),
            package_basis: "fixture".to_owned(),
            normalized_quantity: "1".to_owned(),
            normalized_unit: "kg".to_owned(),
            normalized_price: price.to_owned(),
            normalized_price_unit: "RUB/kg".to_owned(),
            collected_at: Utc.timestamp_opt(0, 0).unwrap(),
            source: "fixture".to_owned(),
            price_context: "Generic".to_owned(),
            availability_status: "observed".to_owned(),
            validation_status: "CandidateObservation".to_owned(),
        }
    }

    fn budget(min: &str, target: &str, max: &str) -> Budget {
        Budget {
            minimum_wage: Decimal::from(1000),
            target: Decimal::from_str(target).unwrap(),
            minimum: Decimal::from_str(min).unwrap(),
            maximum: Decimal::from_str(max).unwrap(),
        }
    }

    #[test]
    fn maximizes_groups_and_obeys_one_per_group() {
        let catalog = ProductCatalog {
            schema_version: 1,
            generated_at: String::new(),
            source: "fixture".to_owned(),
            price_context: "Generic".to_owned(),
            products: vec![
                product("A", 1, "100"),
                product("A", 2, "150"),
                product("B", 3, "200"),
                product("C", 4, "300"),
            ],
        };
        let basket = optimize(&catalog, &budget("590", "600", "610")).unwrap();
        assert_eq!(basket.items.len(), 3);
        assert_eq!(basket.total_rub, "600");
        assert_eq!(
            basket.items.iter().filter(|item| item.group == "A").count(),
            1
        );
    }

    #[test]
    fn optimization_is_deterministic() {
        let catalog = ProductCatalog {
            schema_version: 1,
            generated_at: String::new(),
            source: "fixture".to_owned(),
            price_context: "Generic".to_owned(),
            products: vec![product("A", 2, "100"), product("A", 1, "100")],
        };
        let first = optimize(&catalog, &budget("100", "100", "100")).unwrap();
        let second = optimize(&catalog, &budget("100", "100", "100")).unwrap();
        assert_eq!(
            first.items[0].source_product_id,
            second.items[0].source_product_id
        );
    }

    #[test]
    fn reports_impossible_budget() {
        let catalog = ProductCatalog {
            schema_version: 1,
            generated_at: String::new(),
            source: "fixture".to_owned(),
            price_context: "Generic".to_owned(),
            products: vec![product("A", 1, "100")],
        };
        assert!(optimize(&catalog, &budget("500", "550", "600")).is_err());
    }

    #[test]
    fn balanced_strategy_prefers_lower_concentration() {
        let catalog = ProductCatalog {
            schema_version: 1,
            generated_at: String::new(),
            source: "fixture".to_owned(),
            price_context: "Generic".to_owned(),
            products: vec![
                product("A", 1, "500"),
                product("A", 2, "300"),
                product("B", 3, "100"),
                product("B", 4, "300"),
            ],
        };
        let result = optimize_balanced(
            &catalog,
            &budget("600", "600", "600"),
            &std::collections::BTreeSet::new(),
        )
        .unwrap();
        assert_eq!(
            result
                .items
                .iter()
                .map(|item| item.package_price.as_str())
                .collect::<Vec<_>>(),
            vec!["300", "300"]
        );
    }
}
