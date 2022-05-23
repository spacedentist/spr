/*
 * Copyright (c) Radical HQ Limited
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! A command-line tool for submitting and updating GitHub Pull Requests from
//! local Git commits that may be amended and rebased. Pull Requests can be
//! stacked to allow for a series of code reviews of interdependent code.

use spr::{error::Result, output::output};

fn main() -> Result<()> {
    if let Err(error) = spr::spr::spr() {
        for message in error.messages() {
            output("🛑", message)?;
        }
        std::process::exit(1);
    }

    Ok(())
}
