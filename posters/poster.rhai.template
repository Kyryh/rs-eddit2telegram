const SUBREDDIT = "reddit";
const CHAT = -1001234567890;
const SORT_BY = "hot";
const LIMIT = 10;

fn should_post(submission) {
    true
}

fn should_hide(submission) {
    submission.is_spoiler() || submission.is_nsfw()
}

fn get_text(submission, short) {
    let text = if submission.is_nsfw() { "🔞NSFW🔞\n" } else { "" };

    if !submission.text(short).is_empty() {
        text += "<b>" + submission.title() + "</b>\n\n";

        if should_hide(submission) {
            text += "<tg-spoiler>" + submission.text(short) + "</tg-spoiler>";
        } else {
            text += submission.text(short);
        }

    } else {
        text += submission.title();
    }

    text += "\n\n" + submission.url();

    text
}
