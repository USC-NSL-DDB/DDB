
class MetaStore:
    _store: "MetaStore" = None

    def __init__(self) -> None:
        pass

    @staticmethod
    def inst() -> "MetaStore":
        if MetaStore._store:
            return MetaStore._store 

        MetaStore._store = MetaStore()

    @staticmethod
    def check():
        if MetaStore._store:
            print("NOT EMPTY")
        else:
            print("EMPTY")

# Eager instantiation
_ = MetaStore.inst()

