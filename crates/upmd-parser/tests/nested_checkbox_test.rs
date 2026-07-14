use upmd_parser::Parser;

#[test]
fn test_nested_checkbox_parsing() {
    let text = "- [x] Handle skip command.
- [ ] Syntax highlight.
    - [x] Add syntax highlight crate.
    - [x] Clear highlight after each code block.
- [ ] UI
    - [x] `cat README.md | cargo r --` errors (can't read from stdin). This was fixed in https://github.com/crossterm-rs/crossterm/pull/735.
    - [ ] Home
        - [-] Search code
    - [ ] Menu
";
    let nodes = upmd_parser::new().parse(text).nodes;
    assert_eq!(nodes.len(), 1);
    match &nodes[0] {
        upmd_parser::nodes::Node::List(items) => {
            eprintln!("=== {} items in order ===", items.len());
            for (i, item) in items.iter().enumerate() {
                eprintln!(
                    "[{}] depth={} kind={:?} text={:?}",
                    i, item.depth, item.kind, item.text
                );
            }

            // All items should have their checkbox detected
            let non_task: Vec<_> = items
                .iter()
                .filter(|i| !matches!(i.kind, upmd_parser::nodes::ListKind::Task(_)))
                .collect();
            assert!(
                non_task.is_empty(),
                "Expected all items to be Task kind, got {} non-task items: {:?}",
                non_task.len(),
                non_task.iter().map(|i| &i.text).collect::<Vec<_>>()
            );

            // Parent items must NOT contain nested list source in their text
            for item in items.iter().filter(|i| i.depth == 1) {
                assert!(
                    !item.text.contains("- [x]"),
                    "depth-1 item '{}' should not contain nested checkbox source text",
                    item.text
                );
                assert!(
                    !item.text.contains("- [ ]"),
                    "depth-1 item '{}' should not contain nested checkbox source text",
                    item.text
                );
            }

            // Items should be in parent-before-children order
            for i in 1..items.len() {
                let prev = &items[i - 1];
                let curr = &items[i];
                // A child (higher depth) must follow its parent (lower depth)
                // But a sibling at same or shallower depth is fine
                assert!(
                    curr.depth <= prev.depth || curr.depth == prev.depth + 1,
                    "order violation: item[{}] depth={} follows item[{}] depth={}",
                    i,
                    curr.depth,
                    i - 1,
                    prev.depth
                );
            }
        }
        other => panic!("Expected List node, got {:?}", other),
    }
}
