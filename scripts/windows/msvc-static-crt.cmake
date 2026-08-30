# Keep CMake-built native dependencies aligned with Rust's static MSVC CRT.
set(CMAKE_POLICY_DEFAULT_CMP0091 NEW)
set(
    CMAKE_MSVC_RUNTIME_LIBRARY
    "MultiThreaded"
    CACHE STRING "Use the non-debug static MSVC runtime for every configuration."
    FORCE
)

# Older CMake projects can retain their default /MDd profile flags even when
# CMAKE_MSVC_RUNTIME_LIBRARY is set, so replace every profile's CRT selector.
set(CMAKE_C_FLAGS_DEBUG "/MT /Zi /Ob0 /Od /RTC1" CACHE STRING "" FORCE)
set(CMAKE_CXX_FLAGS_DEBUG "/MT /Zi /Ob0 /Od /RTC1" CACHE STRING "" FORCE)
set(CMAKE_C_FLAGS_RELEASE "/MT /O2 /Ob2 /DNDEBUG" CACHE STRING "" FORCE)
set(CMAKE_CXX_FLAGS_RELEASE "/MT /O2 /Ob2 /DNDEBUG" CACHE STRING "" FORCE)
set(CMAKE_C_FLAGS_RELWITHDEBINFO "/MT /Zi /O2 /Ob1 /DNDEBUG" CACHE STRING "" FORCE)
set(CMAKE_CXX_FLAGS_RELWITHDEBINFO "/MT /Zi /O2 /Ob1 /DNDEBUG" CACHE STRING "" FORCE)
set(CMAKE_C_FLAGS_MINSIZEREL "/MT /O1 /Ob1 /DNDEBUG" CACHE STRING "" FORCE)
set(CMAKE_CXX_FLAGS_MINSIZEREL "/MT /O1 /Ob1 /DNDEBUG" CACHE STRING "" FORCE)
