//! The results of a test run, printed at the end

use std::time::Duration;

pub enum Outcome {
    Passed,
    Failed(String),
}

pub struct Report {
    results: Vec<(String, Outcome, Duration)>,
}

impl Report {
    pub fn new() -> Self {
        Report {
            results: Vec::new(),
        }
    }

    pub fn add(&mut self, name: &str, outcome: Outcome, duration: Duration) {
        match &outcome {
            Outcome::Passed => {
                println!("  ✅ {name} ({}s)", duration.as_secs())
            }
            Outcome::Failed(message) => {
                println!("  ❌ {name} ({}s)", duration.as_secs());
                for line in message.lines() {
                    println!("       {line}");
                }
            }
        }
        self.results.push((name.to_string(), outcome, duration));
    }

    pub fn failed(&self) -> usize {
        self.results
            .iter()
            .filter(|(_, outcome, _)| matches!(outcome, Outcome::Failed(_)))
            .count()
    }

    /// Print the summary. Returns whether everything passed.
    pub fn print_summary(&self) -> bool {
        let failed = self.failed();
        let total = self.results.len();
        println!();
        if failed == 0 {
            match total {
                1 => println!("The test passed."),
                _ => println!("All {total} tests passed."),
            }
        } else {
            println!("{failed} of {total} tests failed:");
            for (name, outcome, _) in &self.results {
                if let Outcome::Failed(_) = outcome {
                    println!("  {name}");
                }
            }
        }
        failed == 0
    }
}
