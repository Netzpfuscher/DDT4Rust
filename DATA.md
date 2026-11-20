# Data policy

This repository does **not** include ddt4all's ECU database (`ecu.zip`) or any extracted copy of it
(`data/`). The database is distributed separately by the ddt4all project and is not redistributed
here.

To run DDTRust:

1. Obtain `ecu.zip` from the ddt4all project.
2. Place it next to the executable, extract it into `data/ecu`, or pass
   `--data /path/to/ecu.zip` / set `DDTRUST_DATA`.

`assets/projects.json` is vehicle/addressing metadata from ddt4all (GPL-3.0-or-later) and is included
so the vehicle list and CAN addressing work out of the box.

Small codec test vectors under `crates/ddtrust-core/tests/fixtures/` are derived test data used to
check compatibility with the ddt4all value codec. They are not a copy of the ECU database. The full
database parse test is expected to be run locally with `--ignored`.
