import os

from ddb_api import DdbClient


def new_client() -> DdbClient:
    return DdbClient(
        os.environ.get("DDB_ENDPOINT", "http://127.0.0.1:5000"),
        bearer_token=os.environ.get("DDB_API_TOKEN"),
    )
