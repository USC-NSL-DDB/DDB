

from abc import ABC

from iddb.utils import ip_int2ip_str


class FrameWorkAdapter(ABC):
    def __init__(self):
        pass

    def get_bt_command_name(self):
        pass

    def extract_id_from_metaddata(self):
        pass
    

class GRPCAdapter(FrameWorkAdapter):
    def __init__(self):
        pass

    def get_bt_command_name(self):
        # return "-grpc-bt-remote"
        return "-get-remote-bt"

    def extract_id_from_metaddata(self, metadata):
        pid, ip_int = int(metadata.get('pid',0)), int(metadata.get('ip',0))
        return f"{ip_int2ip_str(ip_int)}:-{pid}" if 0 <= ip_int <= 0xFFFFFFFF else pid

class ServiceWeaverAdapter(FrameWorkAdapter):
    def __init__(self):
        pass

    def get_bt_command_name(self):
        return "-serviceweaver-bt-remote"

    def extract_id_from_metaddata(self, metadata):
        ip_int=int(metadata.get('ip',0))
        return f"{ip_int2ip_str(ip_int)}" if 0 <= ip_int <= 0xFFFFFFFF else -1
    