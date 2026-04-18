use anyhow::{Context, Result, anyhow};
use clap::{Args, Parser, Subcommand};
use taz_lingq_tool::{
    database::Database,
    gui,
    lingq::{LingqClient, UploadRequest},
    taz::{ArticleSummary, TazClient},
};

#[derive(Parser)]
#[command(name = "taz-lingq")]
#[command(about = "Fetch taz.de articles, store them locally, and upload them to LingQ.")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    Gui,
    Sections,
    Browse(BrowseArgs),
    BrowseUrl(BrowseUrlArgs),
    Fetch(FetchArgs),
    Library(LibraryArgs),
    Upload(UploadArgs),
}

#[derive(Args)]
struct BrowseArgs {
    #[arg(long, default_value = "politik")]
    section: String,
    #[arg(long, default_value_t = 20)]
    limit: usize,
}

#[derive(Args)]
struct BrowseUrlArgs {
    #[arg(long)]
    url: String,
    #[arg(long, default_value_t = 20)]
    limit: usize,
}

#[derive(Args)]
struct FetchArgs {
    #[arg(long)]
    url: String,
    #[arg(long)]
    save: bool,
}

#[derive(Args)]
struct LibraryArgs {
    #[arg(long)]
    search: Option<String>,
    #[arg(long)]
    section: Option<String>,
    #[arg(long)]
    only_not_uploaded: bool,
    #[arg(long, default_value_t = 50)]
    limit: usize,
}

#[derive(Args)]
struct UploadArgs {
    #[arg(long)]
    id: i64,
    #[arg(long)]
    api_key: Option<String>,
    #[arg(long, default_value = "de")]
    language: String,
    #[arg(long)]
    collection: Option<i64>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if matches!(cli.command, None | Some(Commands::Gui)) {
        return gui::run().map_err(|err| anyhow!("failed to launch GUI: {err}"));
    }

    let scraper = TazClient::new()?;

    match cli.command.expect("handled gui/default case above") {
        Commands::Gui => unreachable!(),
        Commands::Sections => {
            for section in scraper.sections() {
                println!("{:<12} {:<14} {}", section.id, section.label, section.url);
            }
        }
        Commands::Browse(args) => {
            let section = scraper
                .section_by_id(&args.section)
                .ok_or_else(|| anyhow!("unknown section '{}'", args.section))?;
            let articles = scraper.browse_section(section, args.limit)?;
            print_summaries(&articles);
        }
        Commands::BrowseUrl(args) => {
            let articles = scraper.browse_url(&args.url, None, args.limit)?;
            print_summaries(&articles);
        }
        Commands::Fetch(args) => {
            let article = scraper.fetch_article(&args.url)?;
            println!("Title: {}", article.title);
            if !article.subtitle.is_empty() {
                println!("Subtitle: {}", article.subtitle);
            }
            if !article.author.is_empty() {
                println!("Author: {}", article.author);
            }
            if !article.date.is_empty() {
                println!("Date: {}", article.date);
            }
            println!("Section: {}", article.section);
            println!("Words: {}", article.word_count);
            println!();
            println!("{}", article.clean_text);

            if args.save {
                let db = Database::open_default()?;
                let id = db.save_article(&article)?;
                println!();
                println!("Saved as article #{id}");
            }
        }
        Commands::Library(args) => {
            let db = Database::open_default()?;
            let rows = db.list_articles(
                args.search.as_deref(),
                args.section.as_deref(),
                args.only_not_uploaded,
                args.limit,
            )?;

            for row in rows {
                let uploaded = if row.uploaded_to_lingq {
                    "uploaded"
                } else {
                    "local"
                };
                println!(
                    "#{:<4} {:<8} {:<20} {:>5}w {}",
                    row.id, uploaded, row.section, row.word_count, row.title
                );
                println!("      {}", row.url);
                if !row.lingq_lesson_url.is_empty() {
                    println!("      LingQ: {}", row.lingq_lesson_url);
                }
            }
        }
        Commands::Upload(args) => {
            let db = Database::open_default()?;
            let article = db
                .get_article(args.id)?
                .ok_or_else(|| anyhow!("article #{} not found", args.id))?;

            let api_key = resolve_api_key(args.api_key)?;
            let lingq = LingqClient::new()?;
            let upload = lingq.upload_lesson(&UploadRequest {
                api_key,
                language_code: args.language.clone(),
                collection_id: args.collection,
                title: article.title.clone(),
                text: article.clean_text.clone(),
                original_url: Some(article.url.clone()),
            })?;

            db.mark_uploaded(article.id, upload.lesson_id, &upload.lesson_url)?;

            println!(
                "Uploaded article #{} to LingQ lesson {}",
                article.id, upload.lesson_id
            );
            println!("{}", upload.lesson_url);
        }
    }

    Ok(())
}

fn print_summaries(articles: &[ArticleSummary]) {
    for (index, article) in articles.iter().enumerate() {
        println!("{}. {}", index + 1, article.title);
        println!("   {}", article.url);
        if !article.section.is_empty() {
            println!("   Section: {}", article.section);
        }
        if !article.teaser.is_empty() {
            println!("   {}", article.teaser);
        }
    }
}

fn resolve_api_key(cli_value: Option<String>) -> Result<String> {
    cli_value
        .or_else(|| std::env::var("LINGQ_API_KEY").ok())
        .filter(|value| !value.trim().is_empty())
        .context("provide --api-key or set LINGQ_API_KEY")
}
