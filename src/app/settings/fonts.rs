//! 系统等宽字体枚举。

use super::DEFAULT_FONT_FAMILY;
use std::collections::BTreeSet;

pub(crate) fn mono_font_families() -> Vec<String> {
    let mut families = crate::renderer::monospace_families()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    families.insert(DEFAULT_FONT_FAMILY.to_owned());
    families.into_iter().collect()
}
