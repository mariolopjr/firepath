//! The household fixture as fixed, committed data
//!
//! Two earners with their employers and salary schedules, the account tree their
//! activity posts to, and the weights that bias discretionary spending across
//! expense categories

// The banking emitter reads the earners, the account tree, and the spending
// weights. The investment accounts and the earner names wait for when investment
// are implemented, so allow the fields still untouched in the non-test build
#![allow(dead_code)]

use crate::rng::Rng;

/// Money in whole cents, kept as an integer
type Cents = i64;

/// Whole dollars to cents, so the salary table reads in dollars but stores exact
/// cents
//
// Called only from the const salary tables, where an overflowing multiply is a
// compile-time const-eval error rather than the silent wrap `arithmetic_side_effects`
// guards against, so the lint does not apply here
#[allow(clippy::arithmetic_side_effects)]
const fn dollars(whole: i64) -> Cents {
    whole * 100
}

/// One step of an earner's pay, effective from its calendar year until the next
/// step or the end of the window
#[derive(Debug)]
pub(crate) struct SalaryStep {
    /// Calendar year the rate takes effect
    pub(crate) effective_year: i32,
    /// Gross annual pay in cents
    pub(crate) annual: Cents,
}

/// A working member of the household
#[derive(Debug)]
pub(crate) struct Earner {
    /// Given name, used to label the income account
    pub(crate) name: &'static str,
    /// Employer paying the salary
    pub(crate) employer: &'static str,
    /// Income account the pay posts to
    pub(crate) income_account: &'static str,
    /// Pay rate over time, ascending by year
    pub(crate) salary: &'static [SalaryStep],
}

/// The account tree as ledger-style colon-delimited paths
#[derive(Debug)]
pub(crate) struct Accounts {
    /// Everyday spending account
    pub(crate) checking: &'static str,
    /// Interest-bearing cash reserve
    pub(crate) savings: &'static str,
    /// Revolving credit line, a liability
    pub(crate) credit: &'static str,
    /// Taxable investment account
    pub(crate) brokerage: &'static str,
    /// Tax-advantaged retirement account
    pub(crate) retirement: &'static str,
}

/// A discretionary spending category and its relative likelihood
#[derive(Debug)]
pub(crate) struct CategoryWeight {
    /// Expense account the spend posts to
    pub(crate) account: &'static str,
    /// Relative weight, a larger value is picked more often
    pub(crate) weight: u32,
}

/// The whole household spec
#[derive(Debug)]
pub(crate) struct Household {
    /// The two earners, in a fixed order
    pub(crate) earners: &'static [Earner],
    /// The account tree activity posts to
    pub(crate) accounts: Accounts,
    /// Discretionary spending categories with their weights
    pub(crate) spending: &'static [CategoryWeight],
}

/// First earner's pay, rising with two raises across the window
const ALEX_SALARY: [SalaryStep; 3] = [
    SalaryStep {
        effective_year: 2015,
        annual: dollars(85_000),
    },
    SalaryStep {
        effective_year: 2019,
        annual: dollars(104_000),
    },
    SalaryStep {
        effective_year: 2023,
        annual: dollars(126_000),
    },
];

/// Second earner's pay, a later start and a steeper climb
const RILEY_SALARY: [SalaryStep; 3] = [
    SalaryStep {
        effective_year: 2015,
        annual: dollars(62_000),
    },
    SalaryStep {
        effective_year: 2018,
        annual: dollars(107_000),
    },
    SalaryStep {
        effective_year: 2022,
        annual: dollars(142_000),
    },
];

/// The two earners with fictional employers
const EARNERS: [Earner; 2] = [
    Earner {
        name: "Alex",
        employer: "Meridian Systems",
        income_account: "Income:Salary:Meridian Systems",
        salary: &ALEX_SALARY,
    },
    Earner {
        name: "Riley",
        employer: "Bramble & Co",
        income_account: "Income:Salary:Bramble & Co",
        salary: &RILEY_SALARY,
    },
];

/// Discretionary spending mix, weights chosen to sum to 100 so each reads as a
/// rough percentage
const SPENDING: [CategoryWeight; 8] = [
    CategoryWeight {
        account: "Expenses:Housing",
        weight: 32,
    },
    CategoryWeight {
        account: "Expenses:Groceries",
        weight: 20,
    },
    CategoryWeight {
        account: "Expenses:Dining",
        weight: 12,
    },
    CategoryWeight {
        account: "Expenses:Transport",
        weight: 10,
    },
    CategoryWeight {
        account: "Expenses:Utilities",
        weight: 8,
    },
    CategoryWeight {
        account: "Expenses:Entertainment",
        weight: 7,
    },
    CategoryWeight {
        account: "Expenses:Health",
        weight: 6,
    },
    CategoryWeight {
        account: "Expenses:Shopping",
        weight: 5,
    },
];

impl Household {
    /// The committed household the fixtures are built from
    pub(crate) fn sample() -> Self {
        Self {
            earners: &EARNERS,
            accounts: Accounts {
                checking: "Assets:Checking",
                savings: "Assets:Savings",
                credit: "Liabilities:CreditCard",
                brokerage: "Assets:Brokerage",
                retirement: "Assets:Retirement",
            },
            spending: &SPENDING,
        }
    }

    /// Sum of the spending weights, the range a category draw spans
    ///
    /// Summed in category order and saturating so the total stays deterministic
    /// and cannot overflow
    pub(crate) fn total_weight(&self) -> u32 {
        self.spending
            .iter()
            .fold(0u32, |acc, cat| acc.saturating_add(cat.weight))
    }

    /// Pick a spending category weighted by its share of the total
    ///
    /// Draws once in `0..total_weight`, then walks the categories subtracting each
    /// weight, so the selection is a stable function of the draw. The walk always
    /// lands on a category while the total is positive. The trailing fallbacks fire
    /// only when every weight is zero, the last category if there is one, otherwise
    /// `Expenses:Uncategorized` for an empty set
    pub(crate) fn pick_category(&self, rng: &mut Rng) -> &'static str {
        let mut choice = rng.below(self.total_weight());
        for cat in self.spending {
            if choice < cat.weight {
                return cat.account;
            }
            choice = choice.saturating_sub(cat.weight);
        }
        self.spending
            .last()
            .map_or("Expenses:Uncategorized", |cat| cat.account)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::{ALEX_SALARY, CategoryWeight, Household, dollars};
    use crate::manifest::DEFAULT_SEED;
    use crate::rng::Rng;

    // Two categories with zero weight, so the picker's walk falls through to its
    // last-category fallback. Static so the `&'static` spending field can hold it
    static ZERO_WEIGHTS: [CategoryWeight; 2] = [
        CategoryWeight {
            account: "Expenses:First",
            weight: 0,
        },
        CategoryWeight {
            account: "Expenses:Last",
            weight: 0,
        },
    ];

    // No categories at all, so the picker falls through to its empty-set fallback
    static NO_WEIGHTS: [CategoryWeight; 0] = [];

    #[test]
    fn dollars_scale_whole_dollars_to_exact_cents() {
        // The salary tables read in dollars but store cents, so the helper scales
        // by exactly 100. Called at runtime here since the const tables evaluate
        // it at compile time. Tied to a real step so the helper and the table
        // cannot drift apart
        assert_eq!(dollars(85_000), 8_500_000);
        assert_eq!(dollars(0), 0);
        assert_eq!(dollars(85_000), ALEX_SALARY[0].annual);
    }

    #[test]
    fn the_household_has_two_earners_with_ascending_salaries() {
        let household = Household::sample();
        assert_eq!(household.earners.len(), 2);
        for earner in household.earners {
            assert!(!earner.salary.is_empty());
            // Each schedule must climb, so a later step never pays less
            for pair in earner.salary.windows(2) {
                let [earlier, later] = pair else {
                    unreachable!("windows(2) yields pairs")
                };
                assert!(later.effective_year > earlier.effective_year);
                assert!(later.annual >= earlier.annual);
            }
        }
    }

    #[test]
    fn the_spending_weights_sum_to_one_hundred() {
        assert_eq!(Household::sample().total_weight(), 100);
    }

    /// The first categories picked for the committed seed, locked so a change to
    /// the household weights or the picker is caught here
    const LOCKED_PICKS: [&str; 8] = [
        "Expenses:Utilities",
        "Expenses:Utilities",
        "Expenses:Housing",
        "Expenses:Shopping",
        "Expenses:Groceries",
        "Expenses:Transport",
        "Expenses:Entertainment",
        "Expenses:Groceries",
    ];

    #[test]
    fn category_picks_for_the_committed_seed_are_locked() {
        let household = Household::sample();
        let mut rng = Rng::new(DEFAULT_SEED);
        let picks: Vec<&str> = (0..LOCKED_PICKS.len())
            .map(|_| household.pick_category(&mut rng))
            .collect();
        assert_eq!(picks, LOCKED_PICKS);
    }

    #[test]
    fn every_pick_is_a_known_category() {
        let household = Household::sample();
        let mut rng = Rng::new(DEFAULT_SEED);
        for _ in 0..1000 {
            let account = household.pick_category(&mut rng);
            assert!(household.spending.iter().any(|cat| cat.account == account));
        }
    }

    #[test]
    fn pick_category_falls_back_to_the_last_category_when_every_weight_is_zero() {
        // A zero total leaves the draw range empty, so the walk never returns and
        // the fallback lands on the last category
        let mut household = Household::sample();
        household.spending = &ZERO_WEIGHTS;
        let mut rng = Rng::new(DEFAULT_SEED);
        assert_eq!(household.pick_category(&mut rng), "Expenses:Last");
    }

    #[test]
    fn pick_category_falls_back_to_uncategorized_for_an_empty_set() {
        // With no categories the walk has nothing to return and no last category,
        // so the fallback names it uncategorized
        let mut household = Household::sample();
        household.spending = &NO_WEIGHTS;
        let mut rng = Rng::new(DEFAULT_SEED);
        assert_eq!(household.pick_category(&mut rng), "Expenses:Uncategorized");
    }
}
