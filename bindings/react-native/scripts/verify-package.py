"""Reject incomplete React Native release tarballs before uploading to npm."""

import argparse
import json
import plistlib
import struct
import tarfile


def verify(archive_path, version):
    with tarfile.open(archive_path, "r:gz") as archive:
        def read(path):
            member = archive.getmember(f"package/{path}")
            if not member.isfile() or member.size == 0:
                raise ValueError(f"Missing or empty package file: {path}")
            return archive.extractfile(member).read()

        package = json.loads(read("package.json"))
        if package["name"] != "@cashu/cdk-react-native" or package["version"] != version:
            raise ValueError("Package name/version does not match the release")
        for path in [
            "lib/index.js", "lib/index.d.ts", "src/index.ts",
            "src/generated/cashu_ffi.ts", "src/generated/cashu_ffi-ffi.ts",
            "src/native/NativeCdkReactNative.ts", "src/native/index.ts",
            "cpp/generated/cashu_ffi.cpp", "cpp/generated/cashu_ffi.hpp",
            "cpp/cashu-cdk-react-native.cpp", "cpp/cashu-cdk-react-native.h",
            "android/build.gradle", "android/CMakeLists.txt", "android/cpp-adapter.cpp",
            "android/src/main/AndroidManifest.xml", "android/src/main/AndroidManifestNew.xml",
            "android/src/main/java/org/cashu/cdk/crypto/CdkReactNativeModule.kt",
            "android/src/main/java/org/cashu/cdk/crypto/CdkReactNativePackage.kt",
            "ios/CdkReactNative.h", "ios/CdkReactNative.mm", "CdkReactNative.podspec",
            "react-native.config.js",
        ]:
            read(path)
        for abi, machine in [("arm64-v8a", 183), ("x86_64", 62)]:
            path = f"android/src/main/jniLibs/{abi}/libcashu_ffi.so"
            elf = read(path)
            if len(elf) < 20 or elf[:6] != b"\x7fELF\x02\x01":
                raise ValueError(f"Invalid 64-bit little-endian Android library: {path}")
            if struct.unpack_from("<H", elf, 18)[0] != machine:
                raise ValueError(f"Wrong Android library architecture: {path}")
        framework = "CashuCdkReactNativeFramework.xcframework"
        info = plistlib.loads(read(f"{framework}/Info.plist"))
        variants = set()
        for library in info["AvailableLibraries"]:
            if library["SupportedPlatform"] != "ios":
                continue
            if "arm64" not in library["SupportedArchitectures"]:
                raise ValueError("The iOS device and simulator libraries must include arm64")
            variants.add(library.get("SupportedPlatformVariant", "device"))
            path = f'{framework}/{library["LibraryIdentifier"]}/{library["LibraryPath"]}'
            if not read(path).startswith(b"!<arch>\n"):
                raise ValueError(f"Invalid iOS static archive: {path}")
        if not {"device", "simulator"} <= variants:
            raise ValueError("Missing iOS device or simulator library")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive")
    parser.add_argument("--version", required=True)
    args = parser.parse_args()
    verify(args.archive, args.version)
    print(f"Verified complete React Native package {args.version}")
