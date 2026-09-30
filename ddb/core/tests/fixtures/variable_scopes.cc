namespace library {
constexpr int header_constant = 90;
int global_noise = 91;
static int file_noise = 92;
}
volatile int observed;

__attribute__((noinline)) int inspect(int argument) {
  static int local_static = 3;
  int local = 4;
  {
    int expired_local = 5;
    observed = expired_local;
  }
  {
    static int nested_static = 6;
    int nested = 7;
    observed = argument + local + local_static + nested + nested_static; // VARIABLES_MARKER
  }
  return library::header_constant + library::global_noise + library::file_noise;
}
int main() { return inspect(1); }
