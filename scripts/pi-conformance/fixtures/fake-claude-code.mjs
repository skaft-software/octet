#!/usr/bin/env node
import readline from 'node:readline';
import fs from 'node:fs';
const send = value => console.log(JSON.stringify(value));
const log = (kind, frame) => fs.appendFileSync(process.env.HOME + '/fake-claude.jsonl', JSON.stringify({kind, frame}) + '\n');
if (process.argv.includes('auth') && process.argv.includes('status')) {
  log('status'); send({loggedIn: true, authMethod: 'claude.ai', subscriptionType: 'fixture plan'});
  process.exit(0);
}
const usage = {input_tokens: 3, output_tokens: 2, cache_read_input_tokens: 0, cache_creation_input_tokens: 0};
for await (const line of readline.createInterface({input: process.stdin, crlfDelay: Infinity})) {
  const frame = JSON.parse(line);
  log('input', frame);
  if (frame.type === 'control_request' && frame.request?.subtype === 'initialize') {
    send({type:'control_response', response:{subtype:'success',request_id:frame.request_id,response:{models:[]}}});
    continue;
  }
  if (frame.shouldQuery === false) {
    send({type:'result',subtype:'success',is_error:false,num_turns:0,usage});
    continue;
  }
  if (frame.type !== 'user') continue;
  const result = frame.message?.content?.find(part => part.type === 'tool_result');
  const mode = fs.readFileSync(process.env.HOME + '/fixture-mode', 'utf8').trim();
  const useTool = mode !== 'text' && !result;
  const signed = mode === 'signed';
  const blocks = [...(signed ? [{type:'thinking',thinking:'VISIBLE-THINKING-PROBE',signature:'opaque-signature-do-not-export'},
    {type:'redacted_thinking',data:'opaque-redacted-do-not-export'}] : []),
    useTool ? {type:'tool_use',id:'tool_fixture_17',name:'mcp__pi__read',input:{path:'read-me.txt'}} :
    {type:'text',text: result ? 'FIXTURE-TOOL-FOLLOWUP' : 'FIXTURE-PLAIN-TEXT'}];
  const reason = useTool ? 'tool_use' : 'end_turn';
  const event = value => send({type:'stream_event',event:value});
  event({type:'message_start',message:{id:'m_fixture',role:'assistant',content:[],usage}});
  blocks.forEach((block,index)=>{
    event({type:'content_block_start',index,content_block:
      block.type === 'thinking' ? {...block,thinking:'',signature:''} :
      block.type === 'tool_use' ? {...block,input:{}} : block.type === 'text' ? {...block,text:''} : block });
    if (block.type === 'thinking') {
      event({type:'content_block_delta',index,delta:{type:'thinking_delta',thinking:block.thinking}});
      event({type:'content_block_delta',index,delta:{type:'signature_delta',signature:block.signature}});
    } else if (block.type === 'tool_use') {
      event({type:'content_block_delta',index,delta:{type:'input_json_delta',partial_json:JSON.stringify(block.input)}});
    } else if (block.type === 'text') {
      event({type:'content_block_delta',index,delta:{type:'text_delta',text:block.text}});
    }
    event({type:'content_block_stop',index});
  });
  event({type:'message_delta',delta:{stop_reason:reason},usage});
  event({type:'message_stop'});
  send({type:'assistant',message:{id:'m_fixture',role:'assistant',content:blocks,stop_reason:reason,usage}});
  send({type:'result',subtype:useTool?'error_max_turns':'success',is_error:useTool,usage});
}
