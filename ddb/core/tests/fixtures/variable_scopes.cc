#include "variable_scopes_header.h"
namespace library {
int global_noise = 91;
static int file_noise = 92;
}
volatile int observed;
struct Pair { int first; int second; };
Pair global_pair{11, 12};
static Pair file_pair{21, 22};
thread_local int thread_value = 31;
int shadowed = 41;

__attribute__((noinline)) int inspect(int argument) {
  static int local_static = 3;
  int local = 4;
  int shadowed = 51;
  {
    int expired_local = 5;
    observed = expired_local;
  }
  {
    static int nested_static = 6;
    int nested = 7;
    observed = argument + local + local_static + nested + nested_static + shadowed + thread_value + global_pair.first + file_pair.first; // VARIABLES_MARKER
  }
  return library::header_constant + library::global_noise + library::file_noise;
}
