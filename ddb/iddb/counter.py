import threading

# A simple thread-safe counter
class TSCounter:
    def __init__(self, init_val: int = 0):
        self.counter = init_val
        self.lock = threading.Lock()

    def increment(self) -> int:
        with self.lock:
            self.counter += 1
            return self.counter
