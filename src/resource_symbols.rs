//! Non-transitive R from AAPT's owned static table and final application IDs.
//! No XML/source regex, placeholder resource IDs, or mirrored global R classes.
use anyhow::{Context, Result, ensure};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Styleable {
    ids: Vec<u32>,
    indices: BTreeMap<String, u32>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Table {
    ints: BTreeMap<(String, String), u32>,
    styleables: BTreeMap<String, Styleable>,
}
fn identifier(s: &str) -> bool {
    !s.is_empty()
        && (s.as_bytes()[0].is_ascii_alphabetic() || b"_$".contains(&s.as_bytes()[0]))
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_$".contains(&b))
}
fn number(s: &str) -> Result<u32> {
    Ok(if let Some(hex) = s.strip_prefix("0x") {
        u32::from_str_radix(hex, 16)?
    } else {
        s.parse()?
    })
}
impl Table {
    pub(crate) fn merge_contract(&mut self, other: Self) -> Result<()> {
        self.ints.extend(other.ints);
        for (name, array) in other.styleables {
            if let Some(previous) = self.styleables.get(&name) {
                ensure!(
                    previous == &array,
                    "conflicting AAR styleable ABI in shared namespace: {name}"
                );
            } else {
                self.styleables.insert(name, array);
            }
        }
        Ok(())
    }
    /// AAPT/AGP R.txt groups an array with its following index fields. Keeping the
    /// group avoids guessing by underscores (Foo and Foo_Bar can coexist).
    pub(crate) fn parse(text: &str) -> Result<Self> {
        ensure!(
            text.len() <= 32 * 1024 * 1024,
            "resource symbols exceed limit"
        );
        let mut table = Self::default();
        let mut group = None::<String>;
        for (line, raw) in text.lines().enumerate() {
            let raw = raw.trim();
            if raw.is_empty() {
                continue;
            }
            ensure!(line < 200000, "resource symbol count limit");
            let mut parts = raw.split_whitespace();
            let shape = parts.next().context("symbol shape")?;
            let kind = parts.next().context("symbol type")?;
            let name = parts.next().context("symbol name")?;
            let value = parts.collect::<Vec<_>>().join(" ");
            ensure!(!value.is_empty(), "symbol value missing");
            let value = value.as_str();
            ensure!(
                identifier(kind) && identifier(name),
                "invalid resource symbol identifier"
            );
            match (shape, kind) {
                ("int[]", "styleable") => {
                    let inner = value
                        .strip_prefix('{')
                        .and_then(|s| s.strip_suffix('}'))
                        .context("invalid styleable array")?
                        .trim();
                    let ids = if inner.is_empty() {
                        vec![]
                    } else {
                        inner
                            .split(',')
                            .map(|s| number(s.trim()))
                            .collect::<Result<Vec<_>>>()?
                    };
                    ensure!(ids.len() <= 20000, "styleable array limit");
                    ensure!(
                        table
                            .styleables
                            .insert(
                                name.into(),
                                Styleable {
                                    ids,
                                    indices: BTreeMap::new()
                                }
                            )
                            .is_none(),
                        "duplicate styleable array"
                    );
                    group = Some(name.into());
                }
                ("int", "styleable") => {
                    let group = group.as_ref().context("styleable index has no array")?;
                    ensure!(
                        name.starts_with(&format!("{group}_")),
                        "styleable index outside its array group"
                    );
                    let array = table.styleables.get_mut(group).unwrap();
                    let index = number(value)?;
                    ensure!(
                        (index as usize) < array.ids.len(),
                        "styleable index outside array"
                    );
                    ensure!(
                        array.indices.insert(name.into(), index).is_none(),
                        "duplicate styleable index field"
                    );
                }
                ("int", _) => {
                    group = None;
                    ensure!(
                        table
                            .ints
                            .insert((kind.into(), name.into()), number(value)?)
                            .is_none(),
                        "duplicate resource field"
                    );
                }
                _ => anyhow::bail!("unsupported resource symbol shape"),
            }
        }
        for array in table.styleables.values() {
            let indices = array.indices.values().copied().collect::<BTreeSet<_>>();
            ensure!(
                indices.len() == array.ids.len() && array.indices.len() == array.ids.len(),
                "incomplete/aliased styleable indices"
            );
        }
        Ok(table)
    }
    /// Restrict ordinary fields to the owner. Remap each styleable attribute through
    /// the final merged table, never reuse the global (possibly larger) array.
    /// Precompiled AARs can inline indices; their R.txt ordering is a binary ABI.
    pub(crate) fn link(&self, final_table: &Self, binary_contract: Option<&Self>) -> Result<Self> {
        let mut out = Self::default();
        for key in self.ints.keys() {
            let id = *final_table
                .ints
                .get(key)
                .with_context(|| format!("owned resource absent from final table: {key:?}"))?;
            ensure!(id != 0, "unlinked owned resource ID");
            out.ints.insert(key.clone(), id);
        }
        for (name, owned) in &self.styleables {
            let contract = if let Some(binary) = binary_contract {
                let contract = binary
                    .styleables
                    .get(name)
                    .context("AAR R.txt lacks owned styleable")?;
                ensure!(
                    contract.indices.keys().eq(owned.indices.keys()),
                    "AAR R.txt styleable fields disagree with owned resources: {name}"
                );
                contract
            } else {
                owned
            };
            let merged = final_table
                .styleables
                .get(name)
                .context("owned styleable absent from final table")?;
            let mut ids = vec![0; contract.ids.len()];
            for (field, index) in &contract.indices {
                let final_index = *merged
                    .indices
                    .get(field)
                    .with_context(|| format!("owned styleable attribute missing: {field}"))?
                    as usize;
                let id = merged.ids[final_index];
                ensure!(id != 0, "unlinked styleable attribute");
                let old = contract.ids[*index as usize];
                ensure!(
                    old >> 24 != 1 || old == id,
                    "framework styleable ID changed"
                );
                ids[*index as usize] = id;
            }
            out.styleables.insert(
                name.clone(),
                Styleable {
                    ids,
                    indices: contract.indices.clone(),
                },
            );
        }
        Ok(out)
    }
    pub(crate) fn has_styleables(&self) -> bool {
        !self.styleables.is_empty()
    }
    pub(crate) fn java(&self, package: &str) -> Result<String> {
        ensure!(
            package.split('.').count() > 1 && package.split('.').all(identifier),
            "invalid R package"
        );
        let mut kinds = BTreeMap::<String, Vec<String>>::new();
        for ((kind, name), id) in &self.ints {
            kinds.entry(kind.clone()).or_default().push(format!(
                "        public static final int {name} = 0x{id:08x};\n"
            ));
        }
        for (name, array) in &self.styleables {
            let fields = kinds.entry("styleable".into()).or_default();
            fields.push(format!(
                "        public static final int[] {name} = {{ {} }};\n",
                array
                    .ids
                    .iter()
                    .map(|id| format!("0x{id:08x}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            for (field, index) in &array.indices {
                fields.push(format!(
                    "        public static final int {field} = {index};\n"
                ));
            }
        }
        let mut text = format!(
            "package {package};\n\n// Owned resource fields; IDs from the final AAPT application table.\npublic final class R {{\n"
        );
        for (kind, fields) in kinds {
            text.push_str(&format!("    public static final class {kind} {{\n"));
            for field in fields {
                text.push_str(&field);
            }
            text.push_str("    }\n");
        }
        text.push_str("}\n");
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ordinary_fields_are_owned_and_final_ids_are_used() {
        let owned = Table::parse("int string mine 0x0\nint id from_xml 0\n").unwrap();
        let linked=Table::parse("int string mine 0x7f010004\nint string parent 0x7f010005\nint id from_xml 0x7f020003\n").unwrap();
        let java = owned
            .link(&linked, None)
            .unwrap()
            .java("test.owner")
            .unwrap();
        assert!(java.contains("mine = 0x7f010004") && java.contains("from_xml = 0x7f020003"));
        assert!(!java.contains("parent"));
    }
    #[test]
    fn styleable_collision_does_not_copy_global_shape_or_indices() {
        let own=Table::parse("int[] styleable Widget {0x01010098, 0}\nint styleable Widget_android_textColor 0\nint styleable Widget_tone 1\n").unwrap();
        let merged=Table::parse("int[] styleable Widget {0x01010098, 0x7f010000, 0x7f010001}\nint styleable Widget_android_textColor 0\nint styleable Widget_foreign 1\nint styleable Widget_tone 2\n").unwrap();
        let linked = own.link(&merged, None).unwrap();
        assert_eq!(linked.styleables["Widget"].ids, [0x01010098, 0x7f010001]);
        assert_eq!(linked.styleables["Widget"].indices["Widget_tone"], 1);
        assert!(!linked.java("test.owner").unwrap().contains("foreign"));
    }
    #[test]
    fn precompiled_aar_index_order_is_preserved() {
        let own=Table::parse("int[] styleable Widget {0x01010098, 0}\nint styleable Widget_android_textColor 0\nint styleable Widget_tone 1\n").unwrap();
        let binary=Table::parse("int[] styleable Widget {0, 0x01010098}\nint styleable Widget_tone 0\nint styleable Widget_android_textColor 1\n").unwrap();
        let merged=Table::parse("int[] styleable Widget {0x01010098, 0x7f010012}\nint styleable Widget_android_textColor 0\nint styleable Widget_tone 1\n").unwrap();
        let linked = own.link(&merged, Some(&binary)).unwrap();
        assert_eq!(linked.styleables["Widget"].ids, [0x7f010012, 0x01010098]);
        assert_eq!(linked.styleables["Widget"].indices["Widget_tone"], 0);
    }
    #[test]
    fn missing_fields_or_inconsistent_binary_contract_fail() {
        let own = Table::parse("int string mine 0\n").unwrap();
        assert!(own.link(&Table::default(), None).is_err());
        let own = Table::parse("int[] styleable X {0}\nint styleable X_a 0\n").unwrap();
        let merged = Table::parse("int[] styleable X {0x7f010000}\nint styleable X_a 0\n").unwrap();
        assert!(own.link(&merged, Some(&Table::default())).is_err());
    }
    #[test]
    fn malformed_or_duplicate_symbol_tables_fail_closed() {
        for input in [
            "int string a 0\nint string a 0\n",
            "int[] styleable X {0,0}\nint styleable X_a 0\n",
            "int[] styleable X {0}\nint styleable X_a 1\n",
            "int string ../../bad 0",
            "long string a 0",
            "int styleable X_a 0",
            "int[] styleable X {0,0}\nint styleable X_a 0\nint styleable X_b 0\n",
        ] {
            assert!(Table::parse(input).is_err(), "{input}");
        }
    }
    #[test]
    fn grouped_underscores_empty_arrays_and_empty_r_are_preserved() {
        let t=Table::parse("int[] styleable Foo {0x01010098}\nint styleable Foo_Bar_baz 0\nint[] styleable Foo_Bar {}\n").unwrap();
        assert_eq!(t.styleables["Foo"].indices["Foo_Bar_baz"], 0);
        assert!(t.styleables["Foo_Bar"].indices.is_empty());
        assert!(
            Table::default()
                .java("test.empty")
                .unwrap()
                .contains("public final class R")
        );
    }
    #[test]
    fn same_namespace_binary_contracts_merge_only_compatible_arrays() {
        let mut first =
            Table::parse("int string first 0\nint[] styleable X {0}\nint styleable X_a 0\n")
                .unwrap();
        first
            .merge_contract(Table::parse("int string second 0\nint[] styleable Y {}\n").unwrap())
            .unwrap();
        assert_eq!(first.ints.len(), 2);
        assert_eq!(first.styleables.len(), 2);
        assert!(
            first
                .merge_contract(
                    Table::parse("int[] styleable X {0}\nint styleable X_b 0\n").unwrap()
                )
                .is_err()
        );
    }
    #[test]
    fn changed_framework_attribute_id_is_not_silently_rebound() {
        let own =
            Table::parse("int[] styleable X {0x01010098}\nint styleable X_android_textColor 0\n")
                .unwrap();
        let other =
            Table::parse("int[] styleable X {0x01010099}\nint styleable X_android_textColor 0\n")
                .unwrap();
        assert!(own.link(&other, None).is_err());
    }
}
