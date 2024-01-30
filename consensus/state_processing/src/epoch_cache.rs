use crate::common::altair::BaseRewardPerIncrement;
use crate::common::base::SqrtTotalActiveBalance;
use crate::common::{altair, base};
use safe_arith::SafeArith;
use types::epoch_cache::{EpochCache, EpochCacheError, EpochCacheKey};
use types::{ActivationQueue, BeaconState, ChainSpec, EthSpec, ForkName, Hash256};

/// Precursor to an `EpochCache`.
pub struct PreEpochCache {
    epoch_key: EpochCacheKey,
    effective_balances: Vec<u64>,
}

impl PreEpochCache {
    pub fn new_for_next_epoch<E: EthSpec>(state: &BeaconState<E>) -> Result<Self, EpochCacheError> {
        // The decision block root for the next epoch is the latest block root from this epoch.
        let latest_block_header = state.latest_block_header();

        // State root should already have been filled in by `process_slot`, except in the case
        // of a `partial_state_advance`.
        let decision_block_root = latest_block_header.canonical_root();

        let epoch_key = EpochCacheKey {
            epoch: state.next_epoch()?,
            decision_block_root,
        };

        Ok(Self {
            epoch_key,
            effective_balances: Vec::with_capacity(state.validators().len()),
        })
    }

    pub fn push_effective_balance(&mut self, effective_balance: u64) {
        self.effective_balances.push(effective_balance);
    }

    pub fn into_epoch_cache(
        self,
        total_active_balance: u64,
        activation_queue: ActivationQueue,
        spec: &ChainSpec,
    ) -> Result<EpochCache, EpochCacheError> {
        let epoch = self.epoch_key.epoch;
        let sqrt_total_active_balance = SqrtTotalActiveBalance::new(total_active_balance);
        let base_reward_per_increment = BaseRewardPerIncrement::new(total_active_balance, spec)?;

        let effective_balance_increment = spec.effective_balance_increment;
        let max_effective_balance_eth = spec
            .max_effective_balance
            .safe_div(effective_balance_increment)?;

        let mut base_rewards = Vec::with_capacity(max_effective_balance_eth.safe_add(1)? as usize);

        for effective_balance_eth in 0..=max_effective_balance_eth {
            let effective_balance = effective_balance_eth.safe_mul(effective_balance_increment)?;
            let base_reward = if spec.fork_name_at_epoch(epoch) == ForkName::Base {
                base::get_base_reward(effective_balance, sqrt_total_active_balance, spec)?
            } else {
                altair::get_base_reward(effective_balance, base_reward_per_increment, spec)?
            };
            base_rewards.push(base_reward);
        }

        Ok(EpochCache::new(
            self.epoch_key,
            self.effective_balances,
            base_rewards,
            activation_queue,
            spec,
        ))
    }
}

pub fn initialize_epoch_cache<E: EthSpec>(
    state: &mut BeaconState<E>,
    spec: &ChainSpec,
) -> Result<(), EpochCacheError> {
    let current_epoch = state.current_epoch();
    let next_epoch = state.next_epoch().map_err(EpochCacheError::BeaconState)?;
    let epoch_cache: &EpochCache = state.epoch_cache();
    let decision_block_root = state
        .proposer_shuffling_decision_root(Hash256::zero())
        .map_err(EpochCacheError::BeaconState)?;

    if epoch_cache
        .check_validity::<E>(current_epoch, decision_block_root)
        .is_ok()
    {
        // `EpochCache` has already been initialized and is valid, no need to initialize.
        return Ok(());
    }

    state.build_total_active_balance_cache_at(current_epoch, spec)?;
    let total_active_balance = state.get_total_active_balance_at_epoch(current_epoch)?;

    // Collect effective balances and compute activation queue.
    let mut effective_balances = Vec::with_capacity(state.validators().len());
    let mut activation_queue = ActivationQueue::default();

    for (index, validator) in state.validators().iter().enumerate() {
        effective_balances.push(validator.effective_balance());

        // Add to speculative activation queue.
        activation_queue
            .add_if_could_be_eligible_for_activation(index, validator, next_epoch, spec);
    }

    // Compute base rewards.
    let pre_epoch_cache = PreEpochCache {
        epoch_key: EpochCacheKey {
            epoch: current_epoch,
            decision_block_root,
        },
        effective_balances,
    };
    *state.epoch_cache_mut() =
        pre_epoch_cache.into_epoch_cache(total_active_balance, activation_queue, spec)?;

    Ok(())
}