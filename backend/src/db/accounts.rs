use super::*;

/// How long an account rests after a usage limit whose reset time the provider didn't report.
const DEFAULT_LIMIT_BACKOFF_MS: i64 = 60 * 60 * 1000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AiAccount {
    pub id: i64,
    pub provider: String,
    pub label: String,
    pub priority: i64,
    pub enabled: bool,
    pub status: String,
    pub limited_until: Option<i64>,
    pub last_error: Option<String>,
    pub last_used_at: Option<i64>,
    pub created_at: i64,
}

const COLUMNS: &str =
    "id, provider, label, priority, enabled, status, limited_until, last_error, last_used_at, created_at";

fn account(row: &pg::Row) -> Result<AiAccount, pg::Error> {
    Ok(AiAccount {
        id: row.get(0)?,
        provider: row.get(1)?,
        label: row.get(2)?,
        priority: row.get(3)?,
        enabled: row.get::<_, i64>(4)? != 0,
        status: row.get(5)?,
        limited_until: row.get(6)?,
        last_error: row.get(7)?,
        last_used_at: row.get(8)?,
        created_at: row.get(9)?,
    })
}

impl Database {
    pub fn list_ai_accounts(&self) -> Result<Vec<AiAccount>, String> {
        let mut statement = self
            .conn
            .prepare(&format!("SELECT {COLUMNS} FROM ai_accounts ORDER BY priority, id"))
            .map_err(|error| error.to_string())?;
        let rows = statement.query_map([], account).map_err(|error| error.to_string())?;
        rows.collect::<Result<_, _>>().map_err(|error| error.to_string())
    }

    pub fn ai_account(&self, id: i64) -> Result<AiAccount, String> {
        self.conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM ai_accounts WHERE id = $1"),
                [id],
                account,
            )
            .optional()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "Account not found".into())
    }

    pub fn create_ai_account(&self, provider: &str, label: &str) -> Result<AiAccount, String> {
        if !matches!(provider, "claude" | "codex" | "opencode") {
            return Err("Unsupported account provider".into());
        }
        let label = label.trim();
        if label.is_empty() || label.chars().count() > 80 {
            return Err("Account name must have 1 to 80 characters".into());
        }
        let now = now_millis();
        let id: i64 = self.conn.query_row(
            "INSERT INTO ai_accounts (provider, label, priority, created_at, updated_at) VALUES ($1, $2, (SELECT COALESCE(MAX(priority), 0) + 1 FROM ai_accounts), $3, $3) RETURNING id",
            params![provider, label, now],
            |row| row.get(0),
        ).map_err(|error| error.to_string())?;
        self.ai_account(id)
    }

    pub fn delete_ai_account(&self, id: i64) -> Result<(), String> {
        self.conn
            .execute("DELETE FROM ai_accounts WHERE id = $1", [id])
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    pub fn rename_ai_account(&self, id: i64, label: &str) -> Result<AiAccount, String> {
        let label = label.trim();
        if label.is_empty() || label.chars().count() > 80 {
            return Err("Account name must have 1 to 80 characters".into());
        }
        self.conn
            .execute(
                "UPDATE ai_accounts SET label = $2, updated_at = $3 WHERE id = $1",
                params![id, label, now_millis()],
            )
            .map_err(|error| error.to_string())?;
        self.ai_account(id)
    }

    pub fn set_ai_account_enabled(&self, id: i64, enabled: bool) -> Result<AiAccount, String> {
        self.conn
            .execute(
                "UPDATE ai_accounts SET enabled = $2, updated_at = $3 WHERE id = $1",
                params![id, enabled as i64, now_millis()],
            )
            .map_err(|error| error.to_string())?;
        self.ai_account(id)
    }

    /// Rewrites priorities to the given order; every existing account must be listed once.
    pub fn reorder_ai_accounts(&self, ids: &[i64]) -> Result<Vec<AiAccount>, String> {
        let mut current = self
            .list_ai_accounts()?
            .into_iter()
            .map(|account| account.id)
            .collect::<Vec<_>>();
        let mut requested = ids.to_vec();
        current.sort_unstable();
        requested.sort_unstable();
        if current != requested {
            return Err("Account order must list every account exactly once".into());
        }
        let tx = self.conn.unchecked_transaction().map_err(|error| error.to_string())?;
        for (index, id) in ids.iter().enumerate() {
            tx.execute(
                "UPDATE ai_accounts SET priority = $2 WHERE id = $1",
                params![id, index as i64 + 1],
            )
            .map_err(|error| error.to_string())?;
        }
        tx.commit().map_err(|error| error.to_string())?;
        self.list_ai_accounts()
    }

    /// Next account to try: enabled, signed in, not resting, by priority. Accounts whose rest has
    /// elapsed count as available again. The first usable account's provider goes first, so a
    /// limit moves reviews to that provider's other accounts before another provider's.
    pub fn pick_ai_account(&self, excluded: &[i64]) -> Result<Option<AiAccount>, String> {
        let now = now_millis();
        let mut accounts = self.list_ai_accounts()?;
        accounts.retain(|account| account.enabled && account.status != "auth_failed");
        let Some(preferred) = accounts.first().map(|account| account.provider.clone()) else {
            return Ok(None);
        };
        // A stable sort keeps the priority order inside each group.
        accounts.sort_by_key(|account| account.provider != preferred);
        Ok(accounts
            .into_iter()
            .find(|account| account.limited_until.is_none_or(|until| until <= now) && !excluded.contains(&account.id)))
    }

    /// Earliest moment a resting account becomes available again.
    pub fn next_ai_account_reset(&self) -> Result<Option<i64>, String> {
        Ok(self
            .list_ai_accounts()?
            .into_iter()
            .filter(|account| account.enabled && account.status == "limited")
            .filter_map(|account| account.limited_until)
            .min())
    }

    pub fn mark_ai_account_used(&self, id: i64) -> Result<(), String> {
        let now = now_millis();
        self.conn.execute("UPDATE ai_accounts SET status = 'ok', limited_until = NULL, last_error = NULL, last_used_at = $2, updated_at = $2 WHERE id = $1", params![id, now]).map_err(|error| error.to_string())?;
        Ok(())
    }

    pub fn mark_ai_account_limited(&self, id: i64, resets_at: Option<i64>, error: &str) -> Result<i64, String> {
        let now = now_millis();
        let until = resets_at
            .filter(|at| *at > now)
            .unwrap_or(now + DEFAULT_LIMIT_BACKOFF_MS);
        self.conn.execute("UPDATE ai_accounts SET status = 'limited', limited_until = $2, last_error = $3, updated_at = $4 WHERE id = $1", params![id, until, error, now]).map_err(|error| error.to_string())?;
        Ok(until)
    }

    pub fn mark_ai_account_auth_failed(&self, id: i64, error: &str) -> Result<(), String> {
        self.conn.execute("UPDATE ai_accounts SET status = 'auth_failed', limited_until = NULL, last_error = $2, updated_at = $3 WHERE id = $1", params![id, error, now_millis()]).map_err(|error| error.to_string())?;
        Ok(())
    }

    /// Drops accounts and engine choices of providers the app no longer runs, such as Gemini,
    /// including ones an import of an older database brings back.
    /// Returns the ids of the accounts it removed.
    pub fn remove_unsupported_ai_accounts(&self) -> Result<Vec<i64>, String> {
        let mut statement = self
            .conn
            .prepare("DELETE FROM ai_accounts WHERE provider NOT IN ('claude', 'codex', 'opencode') RETURNING id")
            .map_err(|error| error.to_string())?;
        let removed = statement
            .query_map([], |row| row.get::<_, i64>(0))
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        self.conn
            .execute(
                "DELETE FROM settings WHERE key LIKE 'review_engine_%' AND key NOT IN ('review_engine_claude', 'review_engine_codex', 'review_engine_opencode')",
                [],
            )
            .map_err(|error| error.to_string())?;
        Ok(removed)
    }

    /// A fresh sign-in clears any recorded failure.
    pub fn reset_ai_account_status(&self, id: i64) -> Result<(), String> {
        self.conn.execute("UPDATE ai_accounts SET status = 'ok', limited_until = NULL, last_error = NULL, updated_at = $2 WHERE id = $1", params![id, now_millis()]).map_err(|error| error.to_string())?;
        Ok(())
    }
}
