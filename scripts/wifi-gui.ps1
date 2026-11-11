Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Security
[Windows.Forms.Application]::EnableVisualStyles()
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $root 'host\target\release\ioscpy.exe'
$dataDir = Join-Path $env:LOCALAPPDATA 'ioscpy-wifi'
New-Item -ItemType Directory -Path $dataDir -Force | Out-Null
$profileFile = Join-Path $dataDir 'profiles.json'
$askpass = Join-Path $dataDir 'ioscpy-askpass.exe'
$askpassSource = Join-Path $PSScriptRoot 'wifi-askpass.cs'
if (!(Test-Path $askpass) -or (Get-Item $askpass).LastWriteTimeUtc -lt (Get-Item $askpassSource).LastWriteTimeUtc) {
    if (Test-Path $askpass) { Remove-Item $askpass -Force }
    Add-Type -Path $askpassSource -ReferencedAssemblies 'System.Windows.Forms','System.Security' -OutputAssembly $askpass -OutputType ConsoleApplication
}
$profiles = @()
if (Test-Path $profileFile) { try { $profiles = @(Get-Content $profileFile -Raw -Encoding UTF8 | ConvertFrom-Json) } catch { [Windows.Forms.MessageBox]::Show('No se ha podido leer el archivo de perfiles.','ioscpy') | Out-Null } }
$form = New-Object Windows.Forms.Form
$form.Text = 'ioscpy · Conectar iPhone'; $form.Size = New-Object Drawing.Size(470,410); $form.StartPosition = 'CenterScreen'; $form.FormBorderStyle = 'FixedDialog'; $form.MaximizeBox = $false
function LabelAt($text,$x,$y) { $c=New-Object Windows.Forms.Label; $c.Text=$text; $c.Location=New-Object Drawing.Point($x,$y); $c.AutoSize=$true; $form.Controls.Add($c) }
function TextAt($x,$y,$width) { $c=New-Object Windows.Forms.TextBox; $c.Location=New-Object Drawing.Point($x,$y); $c.Width=$width; $form.Controls.Add($c); return $c }
function ButtonAt($text,$x,$y,$width) { $c=New-Object Windows.Forms.Button; $c.Text=$text; $c.Location=New-Object Drawing.Point($x,$y); $c.Size=New-Object Drawing.Size($width,32); $form.Controls.Add($c); return $c }
LabelAt 'Perfil' 24 25
$select=New-Object Windows.Forms.ComboBox; $select.Location=New-Object Drawing.Point(150,22); $select.Width=280; $select.DropDownStyle='DropDownList'; $form.Controls.Add($select)
LabelAt 'Nombre' 24 68; $name=TextAt 150 64 280
LabelAt 'IP del iPhone' 24 108; $address=TextAt 150 104 280
LabelAt 'Usuario SSH' 24 148; $user=TextAt 150 144 280; $user.Text='mobile'
LabelAt 'Contraseña SSH' 24 188; $password=TextAt 150 184 280; $password.UseSystemPasswordChar=$true
$remember=New-Object Windows.Forms.CheckBox; $remember.Text='Guardar contraseña en este usuario de Windows'; $remember.Location=New-Object Drawing.Point(150,218); $remember.AutoSize=$true; $form.Controls.Add($remember)
$save=ButtonAt 'Guardar perfil' 24 258 125
$delete=ButtonAt 'Eliminar perfil' 160 258 125
$connect=ButtonAt 'Conectar' 305 258 125
$status=New-Object Windows.Forms.Label; $status.Location=New-Object Drawing.Point(24,308); $status.Size=New-Object Drawing.Size(406,44); $status.Text='Introduce la IP y conecta por Wi-Fi.'; $form.Controls.Add($status)
function RefreshProfiles { $select.Items.Clear(); [void]$select.Items.Add('(Nuevo perfil)'); foreach ($p in $script:profiles) { [void]$select.Items.Add([string]$p.name) }; $select.SelectedIndex=0 }
$select.Add_SelectedIndexChanged({ $i=$select.SelectedIndex - 1; if ($i -ge 0 -and $i -lt $script:profiles.Count) { $p=$script:profiles[$i]; $name.Text=$p.name; $address.Text=$p.address; $user.Text=$p.user; $password.Text=''; $remember.Checked=[bool]$p.credential; if ($p.credential) { try { $password.Text=[Text.Encoding]::UTF8.GetString([Security.Cryptography.ProtectedData]::Unprotect([Convert]::FromBase64String($p.credential),$null,[Security.Cryptography.DataProtectionScope]::CurrentUser)) } catch { $status.Text='No se puede leer la contraseña guardada en este usuario.' } } } else { $name.Text=''; $address.Text=''; $user.Text='mobile'; $password.Text=''; $remember.Checked=$false } })
function WriteProfiles { ConvertTo-Json -InputObject @($script:profiles) -Depth 4 | Set-Content -Encoding UTF8 $profileFile }
$save.Add_Click({ try { if (!$name.Text.Trim() -or !$address.Text.Trim() -or !$user.Text.Trim()) { throw 'Completa nombre, IP y usuario.' }; $credential=''; if ($remember.Checked -and $password.Text) { $credential=[Convert]::ToBase64String([Security.Cryptography.ProtectedData]::Protect([Text.Encoding]::UTF8.GetBytes($password.Text),$null,[Security.Cryptography.DataProtectionScope]::CurrentUser)) }; $p=[pscustomobject]@{name=$name.Text.Trim();address=$address.Text.Trim();user=$user.Text.Trim();credential=$credential}; $script:profiles=@($script:profiles | Where-Object { $_.name -ne $p.name }) + @($p); WriteProfiles; $currentPassword=$password.Text; RefreshProfiles; $select.SelectedItem=$p.name; if (!$remember.Checked) { $password.Text=$currentPassword }; $status.Text='Perfil guardado en este PC.' } catch { [Windows.Forms.MessageBox]::Show($_.Exception.Message,'ioscpy') | Out-Null } })
$delete.Add_Click({ if ($select.SelectedIndex -gt 0) { $n=[string]$select.SelectedItem; $script:profiles=@($script:profiles | Where-Object { $_.name -ne $n }); WriteProfiles; RefreshProfiles; $status.Text='Perfil eliminado.' } })
$script:tunnel=$null; $script:viewer=$null
$connect.Add_Click({
 try {
    $hostName=$address.Text.Trim(); $login=$user.Text.Trim()
    if ($hostName -notmatch '^[A-Za-z0-9.:-]+$' -or $login -notmatch '^[A-Za-z0-9._-]+$' -or $login.StartsWith('-')) { throw 'Introduce una IP o nombre de host y un usuario válidos.' }
    if (!$password.Text) { throw 'Introduce la contraseña SSH.' }
    if (!(Test-Path $exe)) { throw 'No se encuentra ioscpy.exe. Compila o descarga el ejecutable en host\target\release.' }
    $listener=[Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback,0); $listener.Start(); $port=([Net.IPEndPoint]$listener.LocalEndpoint).Port; $listener.Stop()
    $protected=[Convert]::ToBase64String([Security.Cryptography.ProtectedData]::Protect([Text.Encoding]::UTF8.GetBytes($password.Text),$null,[Security.Cryptography.DataProtectionScope]::CurrentUser))
    $start=New-Object Diagnostics.ProcessStartInfo; $start.FileName='ssh.exe'; $start.UseShellExecute=$false; $start.CreateNoWindow=$true; $start.RedirectStandardError=$true
    $start.Arguments="-N -o ExitOnForwardFailure=yes -o NumberOfPasswordPrompts=1 -o ConnectTimeout=15 -L 127.0.0.1:${port}:127.0.0.1:27183 $login@$hostName"
    $start.EnvironmentVariables['SSH_ASKPASS']=$askpass; $start.EnvironmentVariables['SSH_ASKPASS_REQUIRE']='force'; $start.EnvironmentVariables['DISPLAY']='ioscpy:0'; $start.EnvironmentVariables['IOSCPY_CREDENTIAL']=$protected
    $script:tunnel=[Diagnostics.Process]::Start($start); $err=$script:tunnel.StandardError.ReadToEndAsync(); $deadline=[DateTime]::Now.AddSeconds(60); $connect.Enabled=$false; $status.Text='Conectando; comprueba si hay una solicitud de huella SSH.'
    $ready=$false
    while ([DateTime]::Now -lt $deadline -and !$script:tunnel.HasExited) { [Windows.Forms.Application]::DoEvents(); $client=New-Object Net.Sockets.TcpClient; try { $client.Connect('127.0.0.1',$port); $ready=$true; break } catch {} finally { $client.Dispose() }; Start-Sleep -Milliseconds 200 }
    if (!$ready) { if (!$script:tunnel.HasExited) { $script:tunnel.Kill(); $script:tunnel.WaitForExit() }; throw "No se pudo abrir el túnel SSH. $($err.Result)" }
    $script:viewer=Start-Process -FilePath $exe -ArgumentList "--addr 127.0.0.1:$port" -PassThru
    $status.Text='Conectado. Mantén esta ventana abierta.'
    $timer.Start()
 } catch { $connect.Enabled=$true; $status.Text='No se ha podido conectar.'; [Windows.Forms.MessageBox]::Show($_.Exception.Message,'ioscpy') | Out-Null }
})
$timer=New-Object Windows.Forms.Timer; $timer.Interval=1000; $timer.Add_Tick({ if (($script:viewer -and $script:viewer.HasExited) -or ($script:tunnel -and $script:tunnel.HasExited)) { $timer.Stop(); if ($script:tunnel -and !$script:tunnel.HasExited) { $script:tunnel.Kill() }; $connect.Enabled=$true; $status.Text='Sesión finalizada.' } })
$form.Add_FormClosing({ if ($script:tunnel -and !$script:tunnel.HasExited) { $script:tunnel.Kill() }; if ($script:viewer -and !$script:viewer.HasExited) { $script:viewer.CloseMainWindow() | Out-Null } })
RefreshProfiles
[void]$form.ShowDialog()
