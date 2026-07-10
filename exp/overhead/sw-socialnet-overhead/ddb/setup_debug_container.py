#!/usr/bin/env python3
"""Inject ephemeral SSH debug sidecars into every ServiceWeaver app pod.

This is the parameterized form of scripts/setup_debug_container.ipynb: instead
of hand-editing placeholders, pass the kubeconfig path and the app label value
on the command line. Injection is skipped for pods that already carry a sidecar,
so the script is safe to re-run.

Usage:
    python3 setup_debug_container.py --kubeconfig ~/.kube/config --label server.out
"""

import argparse
import sys
from uuid import uuid4

from kubernetes import client, config

DEBUG_IMAGE = "h21565897/distributeddebugger:146"
TARGET_CONTAINER = "serviceweaver"


def has_sidecar(pod) -> bool:
    return bool(pod.spec.ephemeral_containers)


def setup_debug_container(api, pod_name: str, namespace: str, image: str) -> None:
    debug_container = client.V1EphemeralContainer(
        name=f"ssh-debugger{uuid4()}",
        image=image,
        target_container_name=TARGET_CONTAINER,
        image_pull_policy="Always",
        stdin=True,
        tty=True,
        security_context=client.V1SecurityContext(privileged=True),
    )
    api.patch_namespaced_pod_ephemeralcontainers(
        name=pod_name,
        namespace=namespace,
        body={"spec": {"ephemeralContainers": [debug_container]}},
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--kubeconfig", required=True, help="path to a readable kubeconfig")
    parser.add_argument("--label", required=True, help="serviceweaver/app label value, e.g. server.out")
    parser.add_argument("--namespace", default="default")
    parser.add_argument("--image", default=DEBUG_IMAGE, help=f"debugger sidecar image (default: {DEBUG_IMAGE})")
    args = parser.parse_args()

    config.load_kube_config(args.kubeconfig)
    api = client.CoreV1Api()

    selector = f"serviceweaver/app={args.label}"
    pods = api.list_namespaced_pod(namespace=args.namespace, label_selector=selector)
    if not pods.items:
        print(f"No pods matched {selector}", file=sys.stderr)
        return 1

    print(f"Found {len(pods.items)} pods matching {selector}")
    injected = skipped = 0
    for pod in pods.items:
        name = pod.metadata.name
        if has_sidecar(pod):
            print(f"  = {name} (sidecar already present)")
            skipped += 1
            continue
        setup_debug_container(api, name, args.namespace, args.image)
        print(f"  + {name} (sidecar injected)")
        injected += 1

    print(f"\nInjected: {injected} | Already present: {skipped}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
