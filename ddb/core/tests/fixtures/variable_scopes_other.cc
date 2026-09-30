#include "variable_scopes_header.h"
struct Pair { int first; int second; };
static Pair file_pair{71, 72};
int inspect(int argument);
int main() {
  int result = inspect(1);
  return result + file_pair.first + library::header_constant;
}
