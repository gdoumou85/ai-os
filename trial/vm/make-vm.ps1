# WORKSHOP ONLY: builds the desktop edition as a VirtualBox VM (desktop design §3). Rerunnable:
# an existing "ai-os" VM is thrown away and rebuilt from the cloud image. Windows PowerShell 5.1.
param(
  [string]$Repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path,
  [string]$Dir = 'C:\VM\ai-os',
  [string]$Password = 'ai',
  [int]$WaitMinutes = 45
)
$ErrorActionPreference = 'Stop'
$vbm = 'C:\Program Files\Oracle\VirtualBox\VBoxManage.exe'
if (-not (Test-Path $vbm)) { throw "VirtualBox is not installed ($vbm)" }
function VBM { & $vbm @args; if ($LASTEXITCODE -ne 0) { throw "VBoxManage $($args -join ' ') failed ($LASTEXITCODE)" } }
# The same, for the calls whose failure is an answer rather than an error (no such VM, no such
# property). Its own scope keeps $ErrorActionPreference off Stop, which in 5.1 would turn a native
# command's stderr into a terminating error; the lines come back as plain strings either way.
function VBMq { $ErrorActionPreference = 'Continue'; (& $vbm @args 2>&1) | ForEach-Object { "$_" } }

foreach ($b in 'ai-os-engine','ai-os-chat','ai-os-rail') {
  if (-not (Test-Path "$Repo\runtime\target\release\$b")) { throw "$b is not built: run trial/run-tests.sh build --release in WSL first" }
}
New-Item -ItemType Directory -Force $Dir | Out-Null

# 1. The cloud image (26.04 "resolute"; the OVA carries the generic kernel VirtualBox needs).
$ova = "$Dir\resolute-server-cloudimg-amd64.ova"
if (-not (Test-Path $ova)) {
  Write-Host "downloading the Ubuntu 26.04 cloud image (about 800 MB)"
  Start-BitsTransfer -Source 'https://cloud-images.ubuntu.com/resolute/current/resolute-server-cloudimg-amd64.ova' -Destination $ova
}

# 2. Throw away the old machine, before the seed is written: unregistering releases the seed ISO
# from the media registry, so the fresh one is never the file an old machine still holds.
if ((VBMq list vms) -match '^"ai-os" ') {
  VBMq controlvm ai-os poweroff | Out-Null; Start-Sleep 3
  VBM unregistervm ai-os --delete-all
}
if (Test-Path "$Dir\ai-os") { Remove-Item "$Dir\ai-os" -Recurse -Force }

# 3. The seed ISO: user-data with the password filled in, packed with Windows' own IMAPI2.
$seed = "$Dir\seed"; New-Item -ItemType Directory -Force $seed | Out-Null
$ud = (Get-Content "$Repo\trial\vm\user-data" -Raw) -replace '\r', ''
[IO.File]::WriteAllText("$seed\user-data", $ud.Replace('@PASSWORD@', $Password))
[IO.File]::WriteAllText("$seed\meta-data", ((Get-Content "$Repo\trial\vm\meta-data" -Raw) -replace '\r', ''))
Add-Type -TypeDefinition @'
public class IsoWriter {
  public unsafe static void Write(string path, object stream, int blockSize, int totalBlocks) {
    int bytes = 0; byte[] buf = new byte[blockSize]; var ptr = (System.IntPtr)(&bytes);
    var o = System.IO.File.OpenWrite(path);
    var i = stream as System.Runtime.InteropServices.ComTypes.IStream;
    while (totalBlocks-- > 0) { i.Read(buf, blockSize, ptr); o.Write(buf, 0, bytes); }
    o.Flush(); o.Close();
  }
}
'@ -CompilerParameters (New-Object CodeDom.Compiler.CompilerParameters -Property @{ CompilerOptions = '/unsafe' })
$fsi = New-Object -ComObject IMAPI2FS.MsftFileSystemImage
$fsi.FileSystemsToCreate = 3      # ISO9660 + Joliet
$fsi.VolumeName = 'cidata'        # cloud-init's NoCloud label
$fsi.Root.AddTree($seed, $false)
$img = $fsi.CreateResultImage()
$iso = "$Dir\seed.iso"; if (Test-Path $iso) { Remove-Item $iso -Force }
[IsoWriter]::Write($iso, $img.ImageStream, $img.BlockSize, $img.TotalBlocks)

# 4. Import and shape it.
VBM import $ova --vsys 0 --vmname ai-os --basefolder $Dir --cpus 6 --memory 8192
VBM modifyvm ai-os --graphicscontroller vmsvga --vram 128 --accelerate-3d off --clipboard-mode bidirectional --nic1 nat --boot1 disk --boot2 dvd --boot3 none --boot4 none
$info = VBM showvminfo ai-os --machinereadable
# The OVA arrives with three controllers (IDE, SCSI, Floppy) and the root VMDK on the SCSI one, so
# the controller's name comes from the disk's own attachment line; all three then go, and the SATA
# controller below is the machine's only one.
$disk = ($info | Select-String '^"(.+)-(\d+)-(\d+)"="(.+\.vmdk)"').Matches[0]
foreach ($c in ($info | Select-String '^storagecontrollername\d+="(.+)"')) {
  VBM storagectl ai-os --name $c.Matches[0].Groups[1].Value --remove
}
$root = "$Dir\ai-os\root.vdi"
VBM clonemedium disk $disk.Groups[4].Value $root --format VDI
VBM closemedium disk $disk.Groups[4].Value --delete
VBM modifymedium disk $root --resize 65536
$data = "$Dir\ai-os\data.vdi"
VBM createmedium disk --filename $data --size 65536 --format VDI
VBM storagectl ai-os --name SATA --add sata --controller IntelAhci --portcount 3 --bootable on
VBM storageattach ai-os --storagectl SATA --port 0 --device 0 --type hdd --medium $root
VBM storageattach ai-os --storagectl SATA --port 1 --device 0 --type hdd --medium $data
VBM storageattach ai-os --storagectl SATA --port 2 --device 0 --type dvddrive --medium $iso
VBM sharedfolder add ai-os --name repo --hostpath $Repo

# 5. Boot, wait for the guest to say it is set up, then keep a checkpoint of "freshly installed".
$t0 = Get-Date
VBM startvm ai-os
Write-Host "first boot: the desktop and the AI OS are installing inside (typically 20-40 minutes)"
$deadline = $t0.AddMinutes($WaitMinutes)
do {
  Start-Sleep 20
  $ready = (VBMq guestproperty get ai-os /ai-os/setup) -match 'Value: ready'
} until ($ready -or (Get-Date) -gt $deadline)
if (-not $ready) { throw "the guest did not report ready within $WaitMinutes minutes; look at the VM window and /var/log/ai-os-setup.log" }
Write-Host ("set up after {0:n0} s; waiting for the reboot to the login screen" -f ((Get-Date) - $t0).TotalSeconds)
Start-Sleep 90
VBM snapshot ai-os take fresh --live
Write-Host ("login screen after about {0:n0} s. Log in as ai (password: {1}); the rail opens with the session." -f ((Get-Date) - $t0).TotalSeconds, $Password)
