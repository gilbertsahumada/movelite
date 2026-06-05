module counter::counter {
    use std::signer;
    use aptos_framework::event;

    struct Counter has key {
        value: u64,
    }

    #[event]
    struct Incremented has drop, store {
        value: u64,
    }

    /// Abort code raised by `fail_deep`, for the partial-tree abort sample.
    const ETOO_LARGE: u64 = 7;

    public entry fun increment(account: &signer, by: u64) acquires Counter {
        let addr = signer::address_of(account);
        if (!exists<Counter>(addr)) {
            move_to(account, Counter { value: 0 });
        };
        let counter = borrow_global_mut<Counter>(addr);
        counter.value = counter.value + by;
        event::emit(Incremented { value: counter.value });
    }

    /// Aborts a few frames deep, to exercise the partial-tree abort path:
    /// fail_deep -> check -> (signer::address_of) -> abort.
    public entry fun fail_deep(account: &signer, by: u64) {
        check(account, by);
    }

    fun check(account: &signer, by: u64) {
        let _addr = signer::address_of(account);
        assert!(by < 10, ETOO_LARGE);
    }
}
