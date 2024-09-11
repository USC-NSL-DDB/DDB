from typing import Dict, Set, Optional
from threading import Lock

PORT_LOWER_RANGE = 58000
PORT_UPPER_RANGE = 59000

class PortManager:
    __port_mgr: "PortManager" = None

    def __init__(self) -> None:
        self.node_list: Set[int] = set() # right not the type of node list element is str, make it int later
        self.per_node_availble: Dict[int, Set[int]] = {} # for now, the key is str, make it int later
        self._lock = Lock()

    def init_node(self, ip: int):
        with self._lock:
            if not (ip in self.node_list):
                self.node_list.add(ip)
                self.per_node_availble[ip] = set()
                for p in range(PORT_LOWER_RANGE, PORT_UPPER_RANGE):
                    self.per_node_availble[ip].add(p)

    def mark_port(self, ip: int, port: int):
        with self._lock:
            self.per_node_availble[ip].remove(port)
    
    def pick_port(self, ip: int) -> Optional[int]:
        with self._lock:
            if self.per_node_availble[ip]:
                return self.per_node_availble[ip].pop()
            else:
                return None

    def is_port_available(self, ip: int, port: int) -> bool:
        with self._lock: 
            if not ip in self.per_node_availble:
                return True
            return port in self.per_node_availble[ip]

    @staticmethod
    def inst() -> "PortManager":
        if PortManager.__port_mgr == None:
            PortManager.__port_mgr = PortManager()
        return PortManager.__port_mgr

    @staticmethod
    def add_node(ip: int):
        mgr = PortManager.inst()
        mgr.init_node(ip)

    @staticmethod
    def reserve_port(ip: int) -> Optional[int]:
        # ip is a str for now. make it a int later
        PortManager.add_node(ip)
        return PortManager.inst().pick_port(ip)

    @staticmethod
    def mark_port(ip: int, port: int):
        PortManager.add_node(ip)
        PortManager.inst().mark_port(ip, port)

    @staticmethod
    def is_port_available(ip: int, port: int) -> bool:
        return PortManager.inst().is_port_available(ip, port)
