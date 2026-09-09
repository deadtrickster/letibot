use scratch::LruCache;

fn s(x: &str) -> String { x.to_string() }

#[test] fn empty_cache() {
    let mut c = LruCache::new(2);
    assert_eq!(c.len(), 0);
    assert_eq!(c.get(1), None);
}
#[test] fn put_then_get() {
    let mut c = LruCache::new(2);
    c.put(1, s("a"));
    assert_eq!(c.get(1), Some(s("a")));
    assert_eq!(c.len(), 1);
}
#[test] fn evicts_least_recently_used() {
    let mut c = LruCache::new(2);
    c.put(1, s("a"));
    c.put(2, s("b"));
    assert_eq!(c.get(1), Some(s("a")));   // 1 becomes MRU, 2 is LRU
    c.put(3, s("c"));                      // evicts 2
    assert_eq!(c.get(2), None);
    assert_eq!(c.get(1), Some(s("a")));
    assert_eq!(c.get(3), Some(s("c")));
    assert_eq!(c.len(), 2);
}
#[test] fn update_does_not_grow_and_refreshes() {
    let mut c = LruCache::new(2);
    c.put(1, s("a"));
    c.put(2, s("b"));
    c.put(1, s("z"));                      // update: len stays 2, 1 becomes MRU
    assert_eq!(c.len(), 2);
    assert_eq!(c.get(1), Some(s("z")));
    c.put(3, s("c"));                      // evicts 2
    assert_eq!(c.get(2), None);
    assert_eq!(c.get(3), Some(s("c")));
}
#[test] fn capacity_one() {
    let mut c = LruCache::new(1);
    c.put(1, s("a"));
    c.put(2, s("b"));
    assert_eq!(c.get(1), None);
    assert_eq!(c.get(2), Some(s("b")));
    assert_eq!(c.len(), 1);
}
#[test] fn capacity_zero_stores_nothing() {
    let mut c = LruCache::new(0);
    c.put(1, s("a"));
    assert_eq!(c.len(), 0);
    assert_eq!(c.get(1), None);
}
#[test] fn miss_does_not_change_order() {
    let mut c = LruCache::new(2);
    c.put(1, s("a"));
    c.put(2, s("b"));
    assert_eq!(c.get(99), None);           // miss must not refresh anything
    c.put(3, s("c"));                      // 1 is still LRU, evicted
    assert_eq!(c.get(1), None);
    assert_eq!(c.get(2), Some(s("b")));
    assert_eq!(c.get(3), Some(s("c")));
}
#[test] fn eviction_sequence() {
    let mut c = LruCache::new(3);
    for k in 1..=3 { c.put(k, format!("v{}", k)); }
    assert_eq!(c.get(1), Some(s("v1")));   // order now 2,3,1 (LRU first)
    c.put(4, s("v4"));                      // evicts 2
    assert_eq!(c.get(2), None);
    c.put(5, s("v5"));                      // evicts 3
    assert_eq!(c.get(3), None);
    assert_eq!(c.len(), 3);
    assert_eq!(c.get(1), Some(s("v1")));
    assert_eq!(c.get(4), Some(s("v4")));
    assert_eq!(c.get(5), Some(s("v5")));
}
