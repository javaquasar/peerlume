param([Parameter(Mandatory)][string]$BinaryPath)
$ErrorActionPreference = 'Stop'
if (-not $IsWindows) { throw 'Windows resource verification requires Windows.' }
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class PeerLumeIconResources {
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    public static extern IntPtr LoadLibraryEx(string path, IntPtr file, uint flags);
    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern bool FreeLibrary(IntPtr module);
    [DllImport("kernel32.dll", EntryPoint = "FindResourceW", SetLastError = true)]
    public static extern IntPtr FindResource(IntPtr module, IntPtr name, IntPtr type);
    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern uint SizeofResource(IntPtr module, IntPtr resource);
    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern IntPtr LoadResource(IntPtr module, IntPtr resource);
    [DllImport("kernel32.dll")]
    public static extern IntPtr LockResource(IntPtr resource);
}
'@

# LOAD_LIBRARY_AS_DATAFILE: inspect resources without executing the application.
$path = (Resolve-Path -LiteralPath $BinaryPath).Path
$module = [PeerLumeIconResources]::LoadLibraryEx($path, [IntPtr]::Zero, 2)
if ($module -eq [IntPtr]::Zero) { throw 'Could not load executable resources.' }
try {
    function Read-IconResource([int]$Id, [int]$Type) {
        $resource = [PeerLumeIconResources]::FindResource($module, [IntPtr]$Id, [IntPtr]$Type)
        if ($resource -eq [IntPtr]::Zero) { throw "Missing icon resource $Id of type $Type." }
        $size = [PeerLumeIconResources]::SizeofResource($module, $resource)
        $data = [PeerLumeIconResources]::LockResource([PeerLumeIconResources]::LoadResource($module, $resource))
        if ($data -eq [IntPtr]::Zero -or $size -eq 0) { throw 'Empty icon resource.' }
        $bytes = [byte[]]::new($size)
        [System.Runtime.InteropServices.Marshal]::Copy($data, $bytes, 0, $size)
        return ,$bytes
    }

    $group = Read-IconResource 1 14 # RT_GROUP_ICON
    if ($group.Length -lt 6 -or [BitConverter]::ToUInt16($group, 2) -ne 1) { throw 'Invalid icon group.' }
    $count = [BitConverter]::ToUInt16($group, 4)
    $expectedSizes = @(16, 24, 32, 48, 64, 128, 256)
    if ($count -ne $expectedSizes.Count -or $group.Length -ne 6 + 14 * $count) { throw 'Incomplete multi-resolution icon group.' }
    $repository = Split-Path $PSScriptRoot -Parent
    for ($index = 0; $index -lt $count; $index++) {
        $offset = 6 + 14 * $index
        $size = $expectedSizes[$index]
        $dimension = if ($size -eq 256) { 0 } else { $size }
        if ($group[$offset] -ne $dimension -or $group[$offset + 1] -ne $dimension) { throw 'Unexpected icon dimensions.' }
        $id = [BitConverter]::ToUInt16($group, $offset + 12)
        $embedded = Read-IconResource $id 3 # RT_ICON
        $expected = [System.IO.File]::ReadAllBytes((Join-Path $repository "assets/icons/peerlume-$size.png"))
        if ([Convert]::ToBase64String($embedded) -cne [Convert]::ToBase64String($expected)) { throw "Embedded $size-pixel icon does not match the custom artwork." }
    }
    Write-Output "Verified PeerLume executable icon: $($expectedSizes -join ', ') pixels; all embedded PNGs match."
} finally {
    [void][PeerLumeIconResources]::FreeLibrary($module)
}
