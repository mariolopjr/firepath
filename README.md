# firepath - FIRE budgeting, planning, and retirement tool using ledger files

[![Build](https://img.shields.io/github/actions/workflow/status/mariolopjr/firepath/ci.yml?branch=main&style=for-the-badge)](https://github.com/mariolopjr/firepath/actions/workflows/ci.yml)
[![Coverage](https://img.shields.io/codecov/c/github/mariolopjr/firepath/main?style=for-the-badge)](https://codecov.io/gh/mariolopjr/firepath)
[![Ledger Conformance](https://img.shields.io/endpoint?url=https://gist.githubusercontent.com/mariolopjr/97f89c3e0f1234a39f0cc1daae82632f/raw/firepath-conformance.json&style=for-the-badge)](https://github.com/mariolopjr/firepath/actions/workflows/ci.yml)

firepath reads a native [ledger][ledger] journal and turns it into tax-aware retirement analysis: deterministic
projection, [Monte Carlo][monte-carlo], [historical backtest][backtest], [Roth conversion][roth]
planning, and drawdown that models [RMD][rmd], [IRMAA][irmaa], and [NIIT][niit]. Results are presented
via a read-only local dashboard. Journal edits are out-of-scope of the UI (a separation of concerns) and
only supported via the included LSP or the CLI. Everything runs locally and no outbound network access
is required.

> [!IMPORTANT]
> 
> This is super, extremely incomplete. I wouldn't trust this for your finances, not yet anyways. Always
> validate the numbers any financial applications shows you. This application is not financial advice,
> is not validated, and is not liable.
>
> The engine is US-centric for now. LSP will be targeting Neovim but should work with other editors.

[ledger]: https://ledger-cli.org
[monte-carlo]: https://www.investopedia.com/terms/m/montecarlosimulation.asp
[backtest]: https://www.investopedia.com/terms/b/backtesting.asp
[roth]: https://www.investopedia.com/terms/i/iraconversion.asp
[rmd]: https://www.investopedia.com/terms/r/requiredminimumdistribution.asp
[irmaa]: https://www.medicare.gov/basics/costs/medicare-costs
[niit]: https://www.investopedia.com/terms/n/netinvestmentincome.asp

