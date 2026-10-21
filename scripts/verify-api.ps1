param([string]$Address = 'http://127.0.0.1:8848')
$ErrorActionPreference='Stop'
Add-Type -AssemblyName System.Net.Http
$handler=[Net.Http.HttpClientHandler]::new()
$handler.UseProxy=$false
$client=[Net.Http.HttpClient]::new($handler)
$client.Timeout=[TimeSpan]::FromSeconds(120)
function Post-Json($path,$payload){
  $content=[Net.Http.StringContent]::new(($payload | ConvertTo-Json -Depth 8),[Text.Encoding]::UTF8,'application/json')
  try{$response=$client.PostAsync("$Address$path",$content).GetAwaiter().GetResult();$text=$response.Content.ReadAsStringAsync().GetAwaiter().GetResult();return @{status=[int]$response.StatusCode;text=$text}}finally{$content.Dispose()}
}
try {
  # Keep this script ASCII so Windows PowerShell 5.1 and PowerShell 7 decode it identically.
  $chinese=-join ([char[]](0x4F60,0x597D,0xFF0C,0x4E16,0x754C,0x3002))
  $chat=@{model='warshanks/Qwen3-4B-Instruct-2507-AWQ';messages=@(@{role='system';content='Translate to English. Return only the translation.'},@{role='user';content=$chinese});temperature=0;top_p=1;max_tokens=64;stream=$false}
  $reply=Post-Json '/v1/chat/completions' $chat
  $data=$reply.text | ConvertFrom-Json
  if($reply.status -ne 200 -or $data.object -ne 'chat.completion' -or $data.choices[0].message.content -notmatch 'Hello' -or $data.usage.total_tokens -le 0){throw 'JSON generation verification failed'}
  "PASS installed HTTP JSON translation: $($data.choices[0].message.content)"
  $chat.stream=$true
  $reply=Post-Json '/v1/chat/completions' $chat
  if($reply.status -ne 200 -or $reply.text -notmatch 'chat.completion.chunk' -or $reply.text -notmatch 'data: \[DONE\]'){throw 'SSE generation verification failed'}
  'PASS installed HTTP SSE chunks and terminal DONE'
  $reply=Post-Json '/v1/embeddings' @{model='Qwen/Qwen3-Embedding-0.6B';input=@($chinese,'local vector test')}
  $data=$reply.text | ConvertFrom-Json
  if($reply.status -ne 200 -or $data.data.Count -ne 2 -or $data.usage.prompt_tokens -le 0){throw 'Embedding response verification failed'}
  foreach($row in $data.data){
    $squared=0.0
    if($row.embedding.Count -ne 1024){throw 'Unexpected embedding dimension'}
    foreach($value in $row.embedding){if([double]::IsNaN($value) -or [double]::IsInfinity($value)){throw 'Nonfinite vector'};$squared+=$value*$value}
    if([Math]::Abs([Math]::Sqrt($squared)-1) -gt 0.01){throw 'Unnormalized vector'}
  }
  'PASS installed HTTP embeddings: two finite, normalized 1024-dimensional vectors and token usage'
  $reply=Post-Json '/v1/chat/completions' @{model='unknown';messages=@()}
  $errorBody=$reply.text | ConvertFrom-Json
  if($reply.status -lt 400 -or !$errorBody.error.code -or $reply.text -match 'AppData|Desktop|hf_'){throw 'Structured error verification failed'}
  'PASS installed HTTP invalid request returns safe structured error'
} finally {$client.Dispose();$handler.Dispose()}
