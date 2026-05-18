from __future__ import annotations

import argparse
import importlib
import sys
from pathlib import Path
from typing import Sequence

from c_two.crm.descriptor import export_contract_descriptor
from c_two.crm.infer import infer_crm_from_resource


def main(argv: Sequence[str] | None = None) -> int:
    parser = _parser()
    args = parser.parse_args(argv)
    try:
        if args.command == 'export':
            contract = _load_contract(args.target)
            payload = export_contract_descriptor(
                contract,
                methods=args.method or None,
                pretty=args.pretty,
            )
            _write_payload(payload, args.out)
            return 0
        if args.command == 'infer':
            resource = _load_contract(args.target)
            contract = infer_crm_from_resource(
                resource,
                namespace=args.namespace,
                version=args.version,
                name=args.name,
                methods=args.method,
            )
            payload = export_contract_descriptor(
                contract,
                methods=args.method,
                pretty=args.pretty,
            )
            _write_payload(payload, args.out)
            return 0
    except Exception as exc:
        print(f'c-two contract {args.command} failed: {exc}', file=sys.stderr)
        return 2
    parser.error(f'unsupported command {args.command!r}')
    return 2


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog='python -m c_two.cli.contract')
    subparsers = parser.add_subparsers(dest='command', required=True)
    export = subparsers.add_parser('export')
    export.add_argument('target', help='Python CRM class target as module:ClassName')
    export.add_argument('--method', action='append', default=[], help='Limit export to one CRM method; repeatable')
    export.add_argument('--out', help='Write descriptor JSON to this file instead of stdout')
    export.add_argument('--pretty', action='store_true', help='Pretty-print descriptor JSON')
    infer = subparsers.add_parser('infer')
    infer.add_argument('target', help='Python resource class target as module:ClassName')
    infer.add_argument('--namespace', required=True, help='CRM namespace for the inferred projection')
    infer.add_argument('--version', required=True, help='CRM version for the inferred projection')
    infer.add_argument('--name', help='CRM class name for the inferred projection')
    infer.add_argument('--method', action='append', required=True, help='Public resource method to expose; repeatable')
    infer.add_argument('--out', help='Write descriptor JSON to this file instead of stdout')
    infer.add_argument('--pretty', action='store_true', help='Pretty-print descriptor JSON')
    return parser


def _load_contract(target: str) -> type:
    module_name, sep, attr_name = target.partition(':')
    if not sep or not module_name or not attr_name:
        raise ValueError('target must use module:ClassName syntax.')
    module = importlib.import_module(module_name)
    value = module
    for part in attr_name.split('.'):
        value = getattr(value, part)
    if not isinstance(value, type):
        raise TypeError(f'{target} did not resolve to a class.')
    return value


def _write_payload(payload: str, out: str | None) -> None:
    if out:
        Path(out).write_text(payload)
    else:
        sys.stdout.write(payload)
        if not payload.endswith('\n'):
            sys.stdout.write('\n')


if __name__ == '__main__':
    raise SystemExit(main())
