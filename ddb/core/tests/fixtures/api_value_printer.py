"""A lazy nested value used by the canonical API's real GDB scenario."""
import gdb


class RequestPrinter:
    def __init__(self, value):
        self.value = value

    def to_string(self):
        return "DDB test request"

    def children(self):
        yield "headers", self.value["headers"]
        yield "flags", self.value["flags"]


class ArrayPrinter:
    def __init__(self, value):
        self.value = value

    def to_string(self):
        return "DDB test array"

    def display_hint(self):
        return "array"

    def children(self):
        low, high = self.value.type.range()
        for index in range(low, high + 1):
            yield str(index), self.value[index]


def lookup(value):
    if str(value.type).endswith("::DebugRequest"):
        return RequestPrinter(value)
    if value.type.code == gdb.TYPE_CODE_ARRAY:
        return ArrayPrinter(value)
    return None


for objfile in gdb.objfiles():
    objfile.pretty_printers.insert(0, lookup)
