//! 进程内唯一的系统字体库；扫描系统字体代价很高，所有 FontSystem 都从这里克隆。

use glyphon::{FontSystem, fontdb};
use std::sync::OnceLock;

static SYSTEM_FONTS: OnceLock<fontdb::Database> = OnceLock::new();

fn system_font_database() -> &'static fontdb::Database {
    SYSTEM_FONTS.get_or_init(|| {
        let mut database = fontdb::Database::new();
        database.load_system_fonts();
        database
    })
}

/// 基于共享字体库创建 shaping 用的 FontSystem，避免重复扫描磁盘。
pub(crate) fn font_system() -> FontSystem {
    FontSystem::new_with_locale_and_db("en-US".to_owned(), system_font_database().clone())
}

/// 系统中所有等宽字体族名称（去重、排序）。
pub(crate) fn monospace_families() -> impl Iterator<Item = &'static str> {
    system_font_database()
        .faces()
        .filter(|face| face.monospaced)
        .filter_map(|face| face.families.first().map(|(family, _)| family.as_str()))
}
