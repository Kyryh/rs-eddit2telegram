pub struct PostedSubmissions(sqlite::Connection);

impl PostedSubmissions {
    pub fn new(path: impl AsRef<std::path::Path>) -> Result<Self, sqlite::Error> {
        let sql_conn = sqlite::open(path)?;

        sql_conn.execute(
            "CREATE TABLE IF NOT EXISTS posted_submissions (
                chat TEXT NOT NULL,
                reddit_post TEXT NOT NULL,
                PRIMARY KEY (chat, reddit_post)
            );",
        )?;
        Ok(Self(sql_conn))
    }

    pub fn submission_is_posted(
        &self,
        tg_chat: &str,
        reddit_submission_id: &str,
    ) -> Result<bool, sqlite::Error> {
        let mut select_statement = self.0.prepare("SELECT COUNT(*) AS c FROM posted_submissions WHERE chat = :chat AND reddit_post = :post")?;
        select_statement.bind::<&[(_, sqlite::Value)]>(
            &[
                (":chat", tg_chat.into()),
                (":post", reddit_submission_id.into()),
            ][..],
        )?;
        select_statement.next()?;
        Ok(select_statement.read::<i64, _>("c")? != 0)
    }

    pub fn add_submission(
        &self,
        tg_chat: &str,
        reddit_submission_id: &str,
    ) -> Result<(), sqlite::Error> {
        let mut insert_statement = self
            .0
            .prepare("INSERT INTO posted_submissions (chat, reddit_post) VALUES (:chat, :post)")?;

        insert_statement.bind::<&[(_, sqlite::Value)]>(
            &[
                (":chat", tg_chat.into()),
                (":post", reddit_submission_id.into()),
            ][..],
        )?;
        insert_statement.next()?;
        Ok(())
    }
}
