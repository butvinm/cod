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

    const MIGRATIONS: &[&str] = &[
        sql!(
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
        ),
        // RC review B2, final batch: two of this item's own `serialize()` calls (one from the
        // workspace's throttled background queue, one from its quit-time flush) each spawn
        // their own, independent write; nothing orders those two writes against each other, so
        // the one reflecting an earlier moment (still `kind = "agent"`, before a navigation away)
        // could finish second and clobber the later, correct one (`kind = "external"`). Compare
        // `written_at` so only the write whose call actually happened last is ever kept.
        sql!(ALTER TABLE cod_pages ADD COLUMN written_at INTEGER NOT NULL DEFAULT 0),
    ];
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
        // `written_at` (RC review B2, final batch) is the caller's own clock reading from the
        // moment it decided what to write, not when this statement happens to run: two calls
        // for the same item can finish in either order on the background executor, and the
        // `WHERE` here keeps whichever one was actually requested last, never an earlier one
        // that simply got its write in after.
        pub async fn save_page(
            item_id: ItemId,
            workspace_id: WorkspaceId,
            page_id: String,
            url: String,
            title: Option<String>,
            kind: String,
            written_at: i64
        ) -> Result<()> {
            INSERT INTO cod_pages(item_id, workspace_id, page_id, url, title, kind, written_at)
            VALUES (?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(item_id, workspace_id) DO UPDATE SET
                page_id = excluded.page_id,
                url = excluded.url,
                title = excluded.title,
                kind = excluded.kind,
                written_at = excluded.written_at
            WHERE excluded.written_at >= cod_pages.written_at
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
