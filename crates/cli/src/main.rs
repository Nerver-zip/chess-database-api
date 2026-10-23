use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "codb",
    version,
    about = "Chess Openings DB research & curation toolkit"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(clap::Subcommand, Debug)]
enum Commands {
    /// Ingest a PGN stream and report validation statistics
    Ingest {
        #[arg(short, long)]
        input: String,
    },
}

fn main() {
    let cli = Cli::parse();
    println!("codb initialized: {:?}", cli);
}
