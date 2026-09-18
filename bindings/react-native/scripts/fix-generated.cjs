// Fix integration-template defaults in the pinned UniFFI 0.30 generator.
// Keep these changes reproducible after both host generation and mobile builds.
const fs = require("node:fs");
const path = require("node:path");
const root = path.resolve(__dirname, "..");
const pkg = require("../package.json");
const gradle = path.join(root, "android/build.gradle");
if (fs.existsSync(gradle)) {
  const content = fs
    .readFileSync(gradle, "utf8")
    .replaceAll(
      "DummyLibForAndroid_kotlinVersion",
      "CdkReactNative_kotlinVersion",
    )
    .replace(
      /libraryName = "[^"]+"/,
      `libraryName = "${pkg.codegenConfig.name}"`,
    );
  fs.writeFileSync(gradle, content);
}
const cmake = path.join(root, "android/CMakeLists.txt");
if (fs.existsSync(cmake)) {
  fs.writeFileSync(
    cmake,
    fs
      .readFileSync(cmake, "utf8")
      .replace(
        "cmake_minimum_required(VERSION 3.9.0)",
        "cmake_minimum_required(VERSION 3.22.1)",
      ),
  );
}
