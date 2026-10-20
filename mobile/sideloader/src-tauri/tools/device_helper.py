import argparse
import asyncio
import json
import sys
from pathlib import Path

from pymobiledevice3.lockdown import create_using_usbmux
from pymobiledevice3.services.installation_proxy import InstallationProxyService
from pymobiledevice3.usbmux import list_devices


async def devices_command() -> None:
    devices = await list_devices()
    print(
        json.dumps(
            [
                {
                    "udid": device.serial,
                    "transport": "Wi-Fi" if device.connection_type == "Network" else "USB",
                }
                for device in devices
            ]
        )
    )


async def install_command(udid: str, transport: str, package: Path) -> None:
    connection_type = "Network" if transport == "Wi-Fi" else "USB"
    lockdown = await create_using_usbmux(serial=udid, connection_type=connection_type)
    try:
        await InstallationProxyService(lockdown=lockdown).install_from_local(package)
    finally:
        await lockdown.close()
    print("Installation completed.")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Ripo Team iOS device helper")
    subcommands = parser.add_subparsers(dest="command", required=True)
    subcommands.add_parser("devices")
    install = subcommands.add_parser("install")
    install.add_argument("--udid", required=True)
    install.add_argument("--transport", choices=("USB", "Wi-Fi"), default="USB")
    install.add_argument("package", type=Path)
    return parser.parse_args()


async def main() -> None:
    args = parse_args()
    if args.command == "devices":
        await devices_command()
    else:
        await install_command(args.udid, args.transport, args.package)


if __name__ == "__main__":
    try:
        asyncio.run(main())
    except Exception as error:
        print(str(error), file=sys.stderr)
        raise SystemExit(1) from error
