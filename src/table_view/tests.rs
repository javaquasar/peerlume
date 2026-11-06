use super::*;
#[test]
fn malformed_layout_is_repaired_without_hiding_name_or_duplicating_columns() {
    let columns:TableColumns=serde_json::from_value(serde_json::json!({"order":["ratio","name","ratio","unknown"],"hidden":["name","peers","unknown"],"widths":{"name":999,"download":-5,"ratio":999999,"eta":"bad","progress":88.3}})).unwrap();
    assert_eq!(columns.order[..2], [Column::Ratio, Column::Name]);
    assert_eq!(columns.order.len(), 8);
    assert_eq!(columns.order.iter().collect::<BTreeSet<_>>().len(), 8);
    assert!(columns.is_visible(Column::Name));
    assert!(!columns.is_visible(Column::Peers));
    assert!(!columns.widths.contains_key(&Column::Name));
    assert_eq!(columns.widths[&Column::Download], 48);
    assert_eq!(columns.widths[&Column::Ratio], MAX_COLUMN_WIDTH);
    assert_eq!(columns.widths[&Column::Progress], 88);
    assert!(!columns.widths.contains_key(&Column::Eta));
}
#[test]
fn default_and_edited_layout_round_trip_in_toml_and_json() {
    let mut columns: TableColumns = toml::from_str("").unwrap();
    assert_eq!(columns, TableColumns::default());
    assert!(!columns.set_visible(Column::Name, false));
    columns.set_visible(Column::Peers, false);
    columns.set_width(Column::Progress, 220);
    columns.move_to(Column::Ratio, 0);
    assert_eq!(
        toml::from_str::<TableColumns>(&toml::to_string(&columns).unwrap()).unwrap(),
        columns
    );
    assert_eq!(
        serde_json::from_str::<TableColumns>(&serde_json::to_string(&columns).unwrap()).unwrap(),
        columns
    );
}
#[test]
fn moving_columns_in_both_directions_keeps_exactly_one_of_each() {
    let mut columns = TableColumns::default();
    assert!(columns.move_before(Column::Ratio, Column::Name));
    assert_eq!(columns.order[0], Column::Ratio);
    assert!(columns.move_before(Column::Ratio, Column::Upload));
    assert_eq!(columns.order[4], Column::Ratio);
    assert!(!columns.move_before(Column::Name, Column::Name));
    assert!(!columns.move_to(Column::Name, 0));
    columns.set_width(Column::Progress, 0);
    assert_eq!(columns.widths[&Column::Progress], 80);
    assert!(columns.reset_width(Column::Progress));
    assert!(!columns.reset_width(Column::Progress));
}
