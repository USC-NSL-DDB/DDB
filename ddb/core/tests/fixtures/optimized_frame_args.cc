// Compile with GCC -g -O2. The opaque return becomes an optimized-out inline
// argument, which triggers GDB 15.1's filtered MI argument iteration bug.
__attribute__((noinline, noipa)) void stop_here() {
    asm volatile("" ::: "memory");
}
__attribute__((noinline, noipa)) int opaque() {
    static volatile int value;
    return ++value;
}
__attribute__((always_inline)) inline int worker(int unused, int used) {
    volatile int visible = used + 1;
    stop_here();
    return visible;
}
int main() { return worker(opaque(), 7); }
