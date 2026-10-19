//! Saved page tabs (issue #31): one row per page item of a workspace, with its page ID, last URL and title.
//!
//! The row is written whenever the page's URL or title changes, so a restart restores the last page shown.
//! Rows of items that no longer exist are removed by `delete_unloaded_items` when the workspace is saved.

use db::{
    query,
    sqlez::{domain::Domain, thread_safe_connection::ThreadSafeConnection},
    sqlez_macros::sql,
};
use workspace::{ItemId, WorkspaceDb, WorkspaceId};

pub(crate) const TABLE: &str = "cod_pages";

pub(crate) struct CodPagesDb(ThreadSafeConnection);

impl Domain for CodPagesDb {
    const NAME: &str = stringify!(CodPagesDb);

    const MIGRATIONS: &[&str] = &[sql!(
        CREATE TABLE cod_pages (
            item_id INTEGER NOT NULL,
            workspace_id INTEGER NOT NULL,
            page_id TEXT NOT NULL,
            url TEXT NOT NULL,
            title TEXT,
            kind TEXT NOT NULL,

            PRIMARY KEY(item_id, workspace_id),
            FOREIGN KEY(workspace_id) REFERENCES workspaces(workspace_id)
            ON DELETE CASCADE
        ) STRICT;
    )];
}

db::static_connection!(CodPagesDb, [WorkspaceDb]);

/// A saved page tab, as `deserialize` reads it back.
pub(crate) struct SavedPage {
    pub page_id: String,
    pub url: String,
    pub title: Option<String>,
    pub kind: String,
}

impl CodPagesDb {
    query! {
        pub async fn save_page(
            item_id: ItemId,
            workspace_id: WorkspaceId,
            page_id: String,
            url: String,
            title: Option<String>,
            kind: String
        ) -> Result<()> {
            INSERT OR REPLACE INTO cod_pages(item_id, workspace_id, page_id, url, title, kind)
            VALUES (?, ?, ?, ?, ?, ?)
        }
    }

    query! {
        fn page_row(item_id: ItemId, workspace_id: WorkspaceId) -> Result<Option<(String, String, Option<String>, String)>> {
            SELECT page_id, url, title, kind
            FROM cod_pages
            WHERE item_id = ? AND workspace_id = ?
        }
    }

    // Every saved page: item, workspace, page ID and URL. Used to keep page IDs unique and for `codctl` (#38).
    query! {
        pub fn all_pages() -> Result<Vec<(ItemId, i64, String, String)>> {
            SELECT item_id, workspace_id, page_id, url
            FROM cod_pages
        }
    }

    pub(crate) fn get_page(
        &self,
        item_id: ItemId,
        workspace_id: WorkspaceId,
    ) -> anyhow::Result<Option<SavedPage>> {
        Ok(self
            .page_row(item_id, workspace_id)?
            .map(|(page_id, url, title, kind)| SavedPage {
                page_id,
                url,
                title,
                kind,
            }))
    }
}
