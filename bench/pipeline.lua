-- wrk pipelining script matching TechEmpower methodology.
-- Usage: wrk -s pipeline.lua http://host:port/path -- <pipeline_depth>

local pipelines = 1

function init(args)
   if args[1] ~= nil then
      pipelines = tonumber(args[1])
   end

   local r = {}
   for i = 1, pipelines do
      r[i] = wrk.format(nil)
   end
   req = table.concat(r)
end

function request()
   return req
end
