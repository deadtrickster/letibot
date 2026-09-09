pub struct LruCache { cap: usize, order: Vec<u64>, map: std::collections::HashMap<u64, String> }
impl LruCache {
    pub fn new(capacity: usize) -> Self { LruCache { cap: capacity, order: Vec::new(), map: std::collections::HashMap::new() } }
    fn touch(&mut self, key: u64) {
        if let Some(i) = self.order.iter().position(|&k| k == key) { self.order.remove(i); }
        self.order.push(key);
    }
    pub fn get(&mut self, key: u64) -> Option<String> {
        if self.map.contains_key(&key) { self.touch(key); self.map.get(&key).cloned() } else { None }
    }
    pub fn put(&mut self, key: u64, value: String) {
        if self.cap == 0 { return; }
        if !self.map.contains_key(&key) && self.map.len() >= self.cap {
            let lru = self.order.remove(0);
            self.map.remove(&lru);
        }
        self.map.insert(key, value);
        self.touch(key);
    }
    pub fn len(&self) -> usize { self.map.len() }
}
