"""CRM contract helpers shared by registration and connect paths."""
from __future__ import annotations

from dataclasses import dataclass

from .methods import rpc_method_names


@dataclass(frozen=True)
class CRMContract:
    crm_ns: str
    crm_name: str
    crm_ver: str
    abi_hash: str
    signature_hash: str

    def native_args(self) -> tuple[str, str, str, str, str]:
        return (
            self.crm_ns,
            self.crm_name,
            self.crm_ver,
            self.abi_hash,
            self.signature_hash,
        )


def crm_contract_identity(crm_class: type) -> tuple[str, str, str]:
    crm_ns = getattr(crm_class, '__cc_namespace__', '')
    crm_name = getattr(crm_class, '__cc_name__', '')
    crm_ver = getattr(crm_class, '__cc_version__', '')
    if crm_ns and crm_name and crm_ver:
        return crm_ns, crm_name, crm_ver
    raise ValueError(
        f'{crm_class.__name__} is not a valid CRM contract class '
        '(decorate it with @cc.crm).',
    )


def crm_contract_methods(crm_class: type) -> list[str]:
    return rpc_method_names(crm_class)


def crm_contract(crm_class: type) -> CRMContract:
    crm_ns, crm_name, crm_ver = crm_contract_identity(crm_class)
    methods = crm_contract_methods(crm_class)
    abi_hash, signature_hash = crm_route_contract_hashes(
        crm_ns,
        crm_name,
        crm_ver,
        methods,
        crm_class,
    )
    return CRMContract(
        crm_ns=crm_ns,
        crm_name=crm_name,
        crm_ver=crm_ver,
        abi_hash=abi_hash,
        signature_hash=signature_hash,
    )


def crm_route_contract_hashes(
    crm_ns: str,
    crm_name: str,
    crm_ver: str,
    methods: list[str],
    crm_class: type,
) -> tuple[str, str]:
    actual_ns, actual_name, actual_ver = crm_contract_identity(crm_class)
    if (crm_ns, crm_name, crm_ver) != (actual_ns, actual_name, actual_ver):
        raise ValueError(
            'CRM contract identity arguments do not match the CRM class.',
        )

    from .descriptor import build_contract_fingerprints

    return build_contract_fingerprints(crm_class, methods)
