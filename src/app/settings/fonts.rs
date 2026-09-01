//! 系统等宽字体枚举。

use super::DEFAULT_FONT_FAMILY;
use fontdb::Database;
use std::collections::BTreeSet;

pub(crate) fn mono_font_families() -> Vec<String> {
    let mut database = Database::new();
    database.load_system_fonts();
    let mut families = database
        .faces()
        .filter(|face| face.monospaced)
        .filter_map(|face| face.families.first().map(|(family, _)| family.clone()))
        .collect::<BTreeSet<_>>();
    families.insert(DEFAULT_FONT_FAMILY.to_owned());
    families.into_iter().collect()
}
