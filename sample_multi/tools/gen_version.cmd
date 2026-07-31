@echo off
rem Windows half of the extension-neutral tools/gen_version launcher.
rem cmd.exe resolves this file through PATHEXT when frost.toml invokes
rem tools/gen_version; POSIX shells execute the sibling without an extension.
> "%~1" echo #ifndef FROST_SAMPLE_MULTI_VERSION_H
>> "%~1" echo #define FROST_SAMPLE_MULTI_VERSION_H
>> "%~1" echo #define FROST_SAMPLE_MULTI_VERSION "1"
>> "%~1" echo #endif
