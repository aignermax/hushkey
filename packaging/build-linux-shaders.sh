#!/usr/bin/env bash
# CI build tool only. Ubuntu 22.04 has no glslc package; building it here keeps
# release engines compatible with glibc 2.35 instead of requiring Ubuntu 24.04.
set -euo pipefail
PREFIX="$(pwd)/.shader-tools"
if [ ! -x "$PREFIX/bin/glslc" ]; then
  git clone --depth 1 --branch v2025.1 https://github.com/google/shaderc.git .shaderc-source
  test "$(git -C .shaderc-source rev-parse HEAD)" = 0968768c61d4eb7dd861114412e904bb3d59b7b6
  (cd .shaderc-source && python utils/git-sync-deps)
  cmake -S .shaderc-source -B .shaderc-build -G Ninja \
    -DCMAKE_BUILD_TYPE=Release -DSHADERC_SKIP_TESTS=ON \
    -DSHADERC_SKIP_EXAMPLES=ON -DSHADERC_SKIP_COPYRIGHT_CHECK=ON
  cmake --build .shaderc-build --target glslc --parallel 3
  mkdir -p "$PREFIX/bin"
  cp .shaderc-build/glslc/glslc "$PREFIX/bin/"
  git clone --depth 1 --branch v1.4.321 https://github.com/KhronosGroup/Vulkan-Headers.git .vulkan-headers
  cmake -S .vulkan-headers -B .vulkan-headers/build -DCMAKE_INSTALL_PREFIX="$PREFIX"
  cmake --install .vulkan-headers/build
  cmake -S .shaderc-source/third_party/spirv-headers -B .spirv-headers-build -DCMAKE_INSTALL_PREFIX="$PREFIX"
  cmake --install .spirv-headers-build
fi
"$PREFIX/bin/glslc" --version
echo "$PREFIX/bin" >> "$GITHUB_PATH"
echo "VULKAN_SDK=$PREFIX" >> "$GITHUB_ENV"
echo "CMAKE_PREFIX_PATH=$PREFIX" >> "$GITHUB_ENV"
