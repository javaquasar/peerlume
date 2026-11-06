use super::*;
fn order(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|id| (*id).into()).collect()
}
#[test]
fn ctrl_shift_and_union_use_current_identity_order() {
    let mut selection = TorrentSelection::default();
    let all = order(&["a", "b", "c", "d"]);
    selection.click("b", &all, Modifiers::default());
    selection.click(
        "d",
        &all,
        Modifiers {
            ctrl: true,
            shift: false,
        },
    );
    assert_eq!(selection.selected_ids().collect::<Vec<_>>(), ["b", "d"]);
    selection.click(
        "b",
        &order(&["d", "c", "b", "a"]),
        Modifiers {
            ctrl: false,
            shift: true,
        },
    );
    assert_eq!(
        selection.selected_ids().collect::<Vec<_>>(),
        ["b", "c", "d"]
    );
    selection.click(
        "a",
        &all,
        Modifiers {
            ctrl: true,
            shift: true,
        },
    );
    assert_eq!(selection.len(), 4);
    assert_eq!(selection.focus(), Some("a"));
    assert_eq!(selection.anchor(), Some("d"));
}
#[test]
fn context_preserves_groups_and_filter_never_adds_hidden_targets() {
    let all = order(&["a", "b", "c"]);
    let mut selection = TorrentSelection::default();
    selection.select_visible(&all);
    selection.context("b");
    assert_eq!(selection.len(), 3);
    let visible = order(&["b"]);
    assert_eq!(selection.visible_targets(visible.clone()), ["b"]);
    assert_eq!(
        selection.counts(&visible),
        SelectionCounts {
            visible: 1,
            hidden: 2
        }
    );
    selection.clear_hidden(&visible);
    assert_eq!(selection.selected_ids().collect::<Vec<_>>(), ["b"]);
    selection.context("c");
    assert_eq!(selection.selected_ids().collect::<Vec<_>>(), ["c"]);
}
#[test]
fn missing_anchor_removal_readd_and_empty_lists_do_not_retarget() {
    let mut selection = TorrentSelection::default();
    selection.replace("a");
    selection.click(
        "c",
        &order(&["b", "c"]),
        Modifiers {
            ctrl: true,
            shift: true,
        },
    );
    assert_eq!(selection.selected_ids().collect::<Vec<_>>(), ["c"]);
    selection.remove("c");
    assert!(selection.is_empty());
    assert_eq!(selection.focus(), None);
    assert_eq!(selection.anchor(), None);
    selection.retain_live(["a", "b", "c"]);
    assert!(selection.is_empty());
    selection.initialize("c");
    assert!(
        selection.is_empty(),
        "A deliberate deselection must not be reselected every frame"
    );
    selection.select_visible(&[]);
    assert_eq!(selection.focus(), None);
}
#[test]
fn pruning_preserves_only_live_ids_and_keeps_hidden_focus_safe() {
    let mut selection = TorrentSelection::default();
    selection.initialize("a");
    selection.initialize("b");
    assert!(selection.is_selected("a"));
    selection.click(
        "b",
        &[],
        Modifiers {
            ctrl: true,
            shift: false,
        },
    );
    selection.retain_live(["b", "c"]);
    assert_eq!(selection.selected_ids().collect::<Vec<_>>(), ["b"]);
    assert_eq!(selection.focus(), Some("b"));
    selection.clear();
    assert!(selection.is_empty());
    assert_eq!(selection.anchor(), None);
    assert!(selection.visible_targets(Vec::<String>::new()).is_empty());
}
