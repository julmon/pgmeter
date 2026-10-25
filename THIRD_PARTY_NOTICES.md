# Third-party notices

pgmeter is released under the [PostgreSQL License](LICENSE). The binary and
the reports it generates also contain the third-party components listed below.
Each component remains under its own license, reproduced here as those licenses
require.

## Chart.js 4.4.1

- Website: https://www.chartjs.org
- Source in this repository: `src/executor/assets/chart.umd.min.js`
- How it is used: the file is embedded in the `pgmeter` binary at compile time
  and inlined verbatim into every generated `report.html`, so each report is
  self-contained. The license header at the top of the file is preserved.
- License: MIT

```
Copyright (c) 2014-2023 Chart.js Contributors

Permission is hereby granted, free of charge, to any person obtaining a copy of
this software and associated documentation files (the "Software"), to deal in
the Software without restriction, including without limitation the rights to
use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies
of the Software, and to permit persons to whom the Software is furnished to do
so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## Luau

- Website: https://luau.org
- How it is used: pgmeter executes benchmark scripts with Luau, which the
  `mlua` crate (feature `vendored`, via `luau0-src`) compiles from source and
  links statically into the `pgmeter` binary.
- License: MIT

```
Copyright (c) 2019-2022 Roblox Corporation
Copyright (c) 1994–2019 Lua.org, PUC-Rio.

Permission is hereby granted, free of charge, to any person obtaining a copy of
this software and associated documentation files (the "Software"), to deal in
the Software without restriction, including without limitation the rights to
use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies
of the Software, and to permit persons to whom the Software is furnished to do
so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## mlua, mlua-sys and luau0-src

- Repository: https://github.com/mlua-rs/mlua
- License: MIT, Copyright (c) 2022 Aleksandr Orlenko. The license text is
  identical to the Chart.js and Luau texts above, with this copyright line.

## Rust crate dependencies

Every other dependency is a Rust crate listed in `Cargo.lock` and statically
linked into the `pgmeter` binary. All of them are available under permissive
licenses: MIT, Apache-2.0, BSD-2-Clause, Zlib, ISC, Unlicense, CC0-1.0,
MPL-2.0, BSL-1.0 or Unicode-3.0, alone or as alternatives. Where a crate offers
a choice of licenses, pgmeter uses it under the MIT license when MIT is one of
the options, and under Apache-2.0 otherwise.

To reproduce the full list with versions and licenses for the exact dependency
set you built from:

```
cargo install cargo-license
cargo license
```

## Sample benchmarks

The bundled sample benchmarks under `samples/` are original YAML and Lua
implementations inspired by the public TPC specifications. TPC-B, TPC-C, TPC-H,
TPC-DS and TPCx-IoT are trademarks of the Transaction Processing Performance
Council. The samples are not audited, full-specification TPC benchmarks and
their results are not comparable to published TPC results. See the
[trademark notice](README.md#trademark-notice) in the README.
