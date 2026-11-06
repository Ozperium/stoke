import {test} from 'node:test';
import assert from 'node:assert/strict';
import {sampleScenario,SCENARIOS,DURATIONS,TIMELINES} from '../landing/product-visual-model.mjs';
import {presentationState} from '../landing/ember-flow-model.mjs';
test('rounded public clock cannot advance the actual narrative phase',()=>{for(const s of SCENARIOS)for(let t=0;t<DURATIONS[s];t+=.37){const real=sampleScenario(s,t),shown=presentationState(s,Math.round(t),real.phase);assert.equal(shown.phase,real.phase);assert.equal(shown.policy,real.policy);assert.equal(shown.packet?.path,real.packet?.path)}});
test('half-millisecond boundaries never dispatch or retry early',()=>{for(const s of SCENARIOS)for(const[,start,end]of TIMELINES[s])for(const t of[start,end-.49,end]){const real=sampleScenario(s,t),shown=presentationState(s,Math.round(t),real.phase);assert.equal(shown.phase,real.phase);if(!real.packet)assert.equal(shown.packet,null)}});
test('budget has incoming motion only; failure has no packet; complete stays static',()=>{for(const s of SCENARIOS)for(let t=0;t<=DURATIONS[s];t+=10){const a=presentationState(s,t,sampleScenario(s,t).phase);if(s==='budget'&&a.packet)assert.equal(a.packet.path,'ingress');if(a.phase==='connection-failed'||a.finished)assert.equal(a.packet,null);if(a.packet)assert(a.visiblePaths.includes(a.packet.path))}});

import {wakeDistances} from '../landing/ember-flow-model.mjs';
import {findEmberComposition} from '../landing/ember-flow.mjs';
test('wake follows exactly one bounded path and reverses behind the reply',()=>{for(const response of[false,true])for(let i=0;i<=1000;i++){const progress=i/1000,points=wakeDistances({progress,response},200);assert.equal(points[0],progress*200);assert(points.every(p=>p>=0&&p<=200));assert(points.every(p=>response?p>=points[0]:p<=points[0]))}});
test('invalid geometry never emits NaN or unbounded positions',()=>{for(const n of[0,-1,NaN,Infinity])assert.deepEqual(wakeDistances({progress:.5},n),[]);for(const progress of[-1,NaN,Infinity,2])assert.deepEqual(wakeDistances({progress},200),[]);assert.deepEqual(wakeDistances(null,100),[])});
test('initializer finds both root-self and descendant compositions',()=>{const same={matches:()=>true,querySelector:()=>{throw Error('must use self')}};assert.equal(findEmberComposition(same),same);const child={};assert.equal(findEmberComposition({matches:()=>false,querySelector:()=>child}),child)});
