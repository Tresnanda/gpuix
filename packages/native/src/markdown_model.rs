//! Canonical append parser state. No UI, napi, theme or paint data.
use crate::markdown::parser::{parse_document_defs, Block, ReferenceDefinition};
use std::{ops::Range, sync::Arc};
pub struct Parsed {
    pub source: String,
    pub blocks: Vec<Arc<Block>>,
    pub ranges: Vec<Range<usize>>,
    pub stable_prefix: usize,
    pub parsed_bytes: usize,
    pub references: Vec<ReferenceDefinition>,
}
pub fn parse(source: String, previous: Option<&Parsed>) -> Parsed {
    if let Some(old) = previous.filter(|old| old.source == source) {
        return Parsed {
            source,
            blocks: old.blocks.clone(),
            ranges: old.ranges.clone(),
            stable_prefix: old.blocks.len(),
            parsed_bytes: 0,
            references: old.references.clone(),
        };
    }
    let mut stable_prefix = 0;
    let mut start = 0;
    if let Some(old) = previous.filter(|old| source.starts_with(&old.source)) {
        // A definition can retroactively turn an old literal into a link.
        // Conservatively include definitions split across append boundaries.
        if !source.contains("]:") {
            let last = old.ranges.last().map_or(0, |r| r.start);
            start = old.ranges.iter().rev().find(|r| r.start < last).map_or(0, |r| r.start);
            stable_prefix = old.ranges.iter().take_while(|r| r.start < start).count();
            if stable_prefix > 0
                && !matches!(old.blocks[stable_prefix].as_ref(), Block::List { .. } | Block::BlockQuote { .. })
                && matches!(old.blocks[stable_prefix - 1].as_ref(), Block::List { .. } | Block::BlockQuote { .. })
            {
                let above = old.ranges[stable_prefix - 1].start;
                let between = &source[above..start];
                if between[between.trim_end().len()..].matches('\n').count() < 2 {
                    start = above;
                }
            }
            start = source[..start].rfind('\n').map_or(0, |i| i + 1);
            stable_prefix = old.ranges.iter().take_while(|r| r.start < start).count();
        }
    }
    let (tree, mut ranges, references) = parse_document_defs(&source[start..]);
    for range in &mut ranges {
        range.start += start;
        range.end += start;
    }
    let mut blocks = Vec::new();
    if let Some(old) = previous {
        blocks.extend_from_slice(&old.blocks[..stable_prefix]);
        let mut prefix = old.ranges[..stable_prefix].to_vec();
        prefix.append(&mut ranges);
        ranges = prefix;
    }
    blocks.extend(tree.blocks.into_iter().map(Arc::new));
    let parsed_bytes = source.len() - start;
    Parsed { source, blocks, ranges, stable_prefix, parsed_bytes, references }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::parser::parse_document;
    #[test]
    fn every_append_matches_whole_document() {
        let cases = [
            "# Title\n\n- one\n  continuation\n  - nested **bold**\n\n- two\n\n> quote\n>\n> ```rs\n> let x=1;\n> ```\n\nafter",
            ">a\n|b\n>|-c\n\n- one\n\n3. three\n\n4. four\n\nend",
            "before [link][ref] and 😀é\n\n[ref]: https://example.com \"title\"\n\nnext",
            "   ```rust\nfn main() {\n    // héllo\n}\n   ```\n\n    indented\n    next\n\nafter",
            "| Name | Value |\n|---|---:|\n| **bold** | `code` |\n| more | two |\n\n## Heading\n\n- [x] task\n- [ ] next",
            "<div>\nraw HTML\n</div>\n\n[footnote][x]\n\n[x]: /relative\n",
        ];
        for full in cases {
            let mut old = None;
            for end in full.char_indices().map(|(i, c)| i + c.len_utf8()) {
                let next = parse(full[..end].to_string(), old.as_ref());
                let (expected, ranges) = parse_document(&full[..end]);
                assert_eq!(
                    next.blocks.iter().map(AsRef::as_ref).collect::<Vec<_>>(),
                    expected.blocks.iter().collect::<Vec<_>>(),
                    "prefix {:?}",
                    &full[..end]
                );
                assert_eq!(next.ranges, ranges, "ranges {:?}", &full[..end]);
                if let Some(old) = &old {
                    for i in 0..next.stable_prefix {
                        assert!(Arc::ptr_eq(&old.blocks[i], &next.blocks[i]));
                    }
                }
                old = Some(next);
            }
        }
    }
    #[test]
    fn large_stable_prefix_is_shared_and_noop_parses_nothing() {
        let source = (0..1000).map(|i| format!("## Section {i}\n\nText {i}.\n\n")).collect::<String>();
        let old = parse(source.clone(), None);
        let next = parse(source + "new tail", Some(&old));
        assert!(next.stable_prefix > 1900);
        assert!(next.parsed_bytes < 100);
        let noop = parse(next.source.clone(), Some(&next));
        assert_eq!(noop.parsed_bytes, 0);
        assert_eq!(noop.stable_prefix, next.blocks.len());
    }
    #[test]
    fn mixed_container_append_boundaries_match_canonical_parser() {
        let pieces = [
            "paragraph 😀é\n",
            "\n",
            "- item\n",
            "  continuation\n",
            "> quote\n",
            ">\n",
            "---\n",
            "    code\n",
            "# heading\n",
            "```rs\n",
            "let x=1;\n",
            "```\n",
            "[link][ref]\n",
            "[ref]: /path\n",
            "| a | b |\n",
            "|---|---|\n",
            "<div>\n",
            "</div>\n",
            "[^note]: footnote\n",
            "tail [^note]\n",
        ];
        let mut seed = 0x9e3779b9u64;
        for _ in 0..30 {
            let mut source = String::new();
            for _ in 0..35 {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                source.push_str(pieces[(seed >> 32) as usize % pieces.len()]);
            }
            let mut old = None;
            for end in source.char_indices().map(|(i, c)| i + c.len_utf8()) {
                let next = parse(source[..end].to_string(), old.as_ref());
                let (expected, ranges) = parse_document(&source[..end]);
                assert_eq!(
                    next.blocks.iter().map(AsRef::as_ref).collect::<Vec<_>>(),
                    expected.blocks.iter().collect::<Vec<_>>(),
                    "mixed source {:?}",
                    &source[..end]
                );
                assert_eq!(next.ranges, ranges, "mixed ranges {:?}", &source[..end]);
                old = Some(next);
            }
        }
    }
    #[test]
    fn references_force_global_invalidation() {
        let old = parse("A [link][ref].\n\n## Heading\n\nTail".into(), None);
        let next = parse(format!("{}\n\n[ref]: https://example.com", old.source), Some(&old));
        assert_eq!(next.stable_prefix, 0);
        assert_eq!(next.parsed_bytes, next.source.len());
    }
}

pub fn slots(block: &Block) -> usize {
    match block {
        Block::Paragraph { runs } | Block::Heading { runs, .. } => 4 + runs.iter().map(|r| r.text.len() + 2).sum::<usize>(),
        Block::CodeBlock { code, .. } => code.lines().count() + 4,
        Block::BlockQuote { children } => 4 + children.iter().map(slots).sum::<usize>(),
        Block::List { items, .. } => 4 + items.iter().map(|item| 4 + item.iter().map(slots).sum::<usize>()).sum::<usize>(),
        Block::Table { header, rows, .. } => {
            4 + std::iter::once(header)
                .chain(rows)
                .map(|row| 4 + row.iter().flatten().map(|r| r.text.len() + 4).sum::<usize>())
                .sum::<usize>()
        }
        Block::Rule => 4,
    }
}
